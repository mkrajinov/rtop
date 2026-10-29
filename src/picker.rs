//! `rtop k8s` without `--pod`: choose a namespace and a pod, then open the
//! pod view (User Story 3).
//!
//! Like the pod view, the picker never waits on the cluster: list requests
//! go to the kube thread and their replies are picked up on each tick.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::kube::KubeHandle;
use crate::kube::msg::{ErrorCode, Incoming, PodSummary, Reply, Request};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Namespaces,
    Pods,
}

/// What a key asks the caller to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Open { ns: String, pod: String },
    Quit,
}

#[derive(Clone, Copy)]
enum Pending {
    Namespaces,
    Pods,
}

/// Pod lists are fetched again this often while the picker is shown.
const RELIST: Duration = Duration::from_secs(3);

pub struct Picker {
    k: Option<KubeHandle>,
    pub context: String,
    /// The context came from the kubeconfig, not from `--context`.
    pub implicit_context: bool,
    pub server: String,
    /// `""` stands for all namespaces and is always the first entry.
    pub namespaces: Vec<String>,
    /// Why the namespace list is incomplete, e.g. listing is forbidden.
    pub ns_note: Option<String>,
    /// The namespace whose pods are listed.
    pub ns: String,
    /// `None` until the first answer for `ns` arrives.
    pub pods: Option<Result<Vec<PodSummary>, String>>,
    pub focus: Pane,
    pub ns_sel: usize,
    pub pod_sel: usize,
    pub ns_filter: String,
    pub pod_filter: String,
    pub filtering: bool,
    pub status: Option<(String, Instant)>,
    /// Set by the caller while a pod view is being opened.
    pub connecting: Option<String>,
    /// Unix seconds that ages are measured against (fixed in tests).
    pub now: u64,
    pending: HashMap<u64, Pending>,
    /// The pod to select once the list arrives (the one just left).
    want: Option<(String, String)>,
    /// Only the newest pod request counts: the selection may have moved on.
    pods_req: u64,
    listed: Option<Instant>,
}

impl Picker {
    /// Build the client and list the namespaces. Runs before the TUI, so
    /// errors are plain one-line messages, as with `--pod`.
    pub fn connect(context: Option<String>, namespace: Option<String>) -> anyhow::Result<Self> {
        let mut k = KubeHandle::start();
        let implicit_context = context.is_none();
        let id = k.send(Request::Hello { context });
        let deadline = Instant::now() + Duration::from_secs(15);
        let (context, default_ns, server) = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match k.recv_timeout(left) {
                Some(Incoming::Reply(got, Reply::Hello { context, namespace, server_version })) if got == id => {
                    break (context, namespace, server_version);
                }
                Some(Incoming::Reply(got, Reply::Err(e))) if got == id => bail!("{e}"),
                Some(_) => {}
                None => bail!("the cluster did not answer within 15 s"),
            }
        };
        let mut p = Picker::new(&context, implicit_context, &server, &namespace.unwrap_or(default_ns));
        p.k = Some(k);
        p.request(Request::ListNamespaces, Pending::Namespaces);
        p.list_pods();
        Ok(p)
    }

    /// A picker with no cluster behind it (tests fill in the lists).
    pub fn new(context: &str, implicit_context: bool, server: &str, ns: &str) -> Self {
        Picker {
            k: None,
            context: context.into(),
            implicit_context,
            server: server.into(),
            namespaces: vec![String::new(), ns.to_string()],
            ns_note: None,
            ns: ns.into(),
            pods: None,
            focus: Pane::Pods,
            ns_sel: 1,
            pod_sel: 0,
            ns_filter: String::new(),
            pod_filter: String::new(),
            filtering: false,
            status: None,
            connecting: None,
            now: crate::fmt::now_unix(),
            pending: HashMap::new(),
            want: None,
            pods_req: 0,
            listed: None,
        }
    }

    fn request(&mut self, req: Request, kind: Pending) -> u64 {
        let Some(k) = &mut self.k else { return 0 };
        let id = k.send(req);
        self.pending.insert(id, kind);
        id
    }

    fn list_pods(&mut self) {
        self.pods_req = self.request(Request::ListPods { ns: self.ns.clone() }, Pending::Pods);
        self.listed = Some(Instant::now());
    }

    /// Back from the pod view of `ns/pod`: show its namespace with it
    /// selected, freshly listed.
    pub fn came_back(&mut self, ns: &str, pod: &str) {
        self.want = Some((ns.to_string(), pod.to_string()));
        if self.ns.is_empty() || self.ns == ns {
            self.list_pods();
        } else {
            self.ns_sel = self.visible_namespaces().iter().position(|n| *n == ns).unwrap_or(self.ns_sel);
            self.show_namespace(ns.to_string());
        }
    }

    /// Pick up answers and list the pods again when they are due.
    pub fn tick(&mut self) {
        while let Some(inc) = self.k.as_ref().and_then(|k| k.try_recv()) {
            if let Incoming::Reply(id, reply) = inc {
                match self.pending.remove(&id) {
                    Some(Pending::Namespaces) => self.got_namespaces(reply),
                    Some(Pending::Pods) if id == self.pods_req => self.got_pods(reply),
                    _ => {}
                }
            }
        }
        self.now = crate::fmt::now_unix();
        if self.connecting.is_none() && self.listed.is_none_or(|t| t.elapsed() >= RELIST) {
            self.list_pods();
        }
    }

    pub fn set_namespaces(&mut self, mut names: Vec<String>) {
        if !names.contains(&self.ns) && !self.ns.is_empty() {
            names.push(self.ns.clone());
            names.sort();
        }
        names.insert(0, String::new());
        self.namespaces = names;
        self.ns_sel = self.visible_namespaces().iter().position(|n| **n == self.ns).unwrap_or(0);
    }

    fn got_namespaces(&mut self, reply: Reply) {
        match reply {
            Reply::Namespaces(names) => self.set_namespaces(names),
            // Common for namespace-scoped users: keep the context's
            // namespace and let them type others.
            Reply::Err(e) if e.code == ErrorCode::Forbidden => {
                self.ns_note = Some("you may not list namespaces: type one with /".into())
            }
            Reply::Err(e) => self.ns_note = Some(format!("namespaces: {e}")),
            _ => {}
        }
    }

    pub fn set_pods(&mut self, pods: Result<Vec<PodSummary>, String>) {
        // Keep the same pod selected across refreshes.
        let keep = self.want.take().or_else(|| self.selected_pod().map(|p| (p.ns.clone(), p.name.clone())));
        self.pods = Some(pods);
        if let Some((ns, name)) = keep {
            let rows = self.visible_pods();
            self.pod_sel = rows.iter().position(|p| p.ns == ns && p.name == name).unwrap_or(self.pod_sel);
        }
        self.pod_sel = self.pod_sel.min(self.visible_pods().len().saturating_sub(1));
    }

    fn got_pods(&mut self, reply: Reply) {
        match reply {
            Reply::Pods(p) => self.set_pods(Ok(p)),
            Reply::Err(e) => self.set_pods(Err(e.to_string())),
            _ => {}
        }
    }

    pub fn visible_namespaces(&self) -> Vec<&String> {
        let f = self.ns_filter.to_lowercase();
        self.namespaces.iter().filter(|n| n.is_empty() || n.to_lowercase().contains(&f)).collect()
    }

    pub fn visible_pods(&self) -> Vec<&PodSummary> {
        let f = self.pod_filter.to_lowercase();
        match &self.pods {
            Some(Ok(p)) => p.iter().filter(|p| format!("{}/{}", p.ns, p.name).to_lowercase().contains(&f)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn selected_pod(&self) -> Option<&PodSummary> {
        self.visible_pods().get(self.pod_sel).copied()
    }

    fn show_namespace(&mut self, ns: String) {
        if ns == self.ns {
            return;
        }
        self.ns = ns;
        self.pods = None;
        self.pod_sel = 0;
        self.list_pods();
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    pub fn status_text(&self) -> Option<&str> {
        self.status.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(6)).map(|(m, _)| m.as_str())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if self.connecting.is_some() {
            return Action::None;
        }
        if self.filtering {
            self.key_filter(key);
            return Action::None;
        }
        let len = match self.focus {
            Pane::Namespaces => self.visible_namespaces().len(),
            Pane::Pods => self.visible_pods().len(),
        };
        let sel = match self.focus {
            Pane::Namespaces => self.ns_sel,
            Pane::Pods => self.pod_sel,
        };
        let moved = match key.code {
            KeyCode::Up => Some(sel.saturating_sub(1)),
            KeyCode::Down => Some(sel + 1),
            KeyCode::PageUp => Some(sel.saturating_sub(20)),
            KeyCode::PageDown => Some(sel + 20),
            KeyCode::Home => Some(0),
            KeyCode::End => Some(usize::MAX),
            _ => None,
        };
        if let Some(to) = moved {
            self.select(to.min(len.saturating_sub(1)));
            return Action::None;
        }
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.focus = if self.focus == Pane::Pods { Pane::Namespaces } else { Pane::Pods };
            }
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('r') => self.list_pods(),
            KeyCode::Esc => {
                let f = self.filter_mut();
                if !f.is_empty() {
                    f.clear();
                    self.clamp();
                } else if self.focus == Pane::Pods {
                    self.focus = Pane::Namespaces;
                }
            }
            KeyCode::Enter if self.focus == Pane::Namespaces => self.focus = Pane::Pods,
            KeyCode::Enter => return self.open(),
            _ => {}
        }
        Action::None
    }

    fn filter_mut(&mut self) -> &mut String {
        match self.focus {
            Pane::Namespaces => &mut self.ns_filter,
            Pane::Pods => &mut self.pod_filter,
        }
    }

    fn key_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.filtering = false;
                // A namespace nobody may list can still be typed in full.
                let typed = self.ns_filter.trim().to_string();
                if self.focus == Pane::Namespaces && !typed.is_empty() && self.visible_namespaces().len() == 1 {
                    self.namespaces.push(typed.clone());
                    self.ns_sel = 1;
                    self.show_namespace(typed);
                    self.focus = Pane::Pods;
                }
            }
            KeyCode::Esc => {
                self.filtering = false;
                self.filter_mut().clear();
            }
            KeyCode::Backspace => {
                self.filter_mut().pop();
            }
            KeyCode::Char(c) => self.filter_mut().push(c),
            _ => {}
        }
        self.clamp();
    }

    /// Keep both selections inside their filtered lists; the namespace
    /// under the cursor is the one whose pods are shown.
    fn clamp(&mut self) {
        match self.focus {
            Pane::Namespaces => self.select(self.ns_sel.min(self.visible_namespaces().len().saturating_sub(1))),
            Pane::Pods => self.pod_sel = self.pod_sel.min(self.visible_pods().len().saturating_sub(1)),
        }
    }

    fn select(&mut self, i: usize) {
        match self.focus {
            Pane::Namespaces => {
                self.ns_sel = i;
                if let Some(ns) = self.visible_namespaces().get(i).map(|n| n.to_string()) {
                    self.show_namespace(ns);
                }
            }
            Pane::Pods => self.pod_sel = i,
        }
    }

    /// Open the selected pod, or say why it cannot be opened.
    fn open(&mut self) -> Action {
        let Some(p) = self.selected_pod() else { return Action::None };
        if p.phase != "Running" {
            let why = p.reason.clone().unwrap_or_else(|| "it has no running containers".into());
            let msg = format!("{}/{} is {}: {why}; rtop reads running containers only", p.ns, p.name, p.phase);
            self.set_status(msg);
            return Action::None;
        }
        Action::Open { ns: p.ns.clone(), pod: p.name.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(ns: &str, name: &str, phase: &str) -> PodSummary {
        PodSummary { ns: ns.into(), name: name.into(), phase: phase.into(), containers: 1, ..Default::default() }
    }

    fn picker() -> Picker {
        let mut p = Picker::new("kind-rtop", false, "v1.32.2", "rtop-demo");
        p.set_namespaces(vec!["default".into(), "kube-system".into(), "rtop-demo".into()]);
        p.set_pods(Ok(vec![pod("rtop-demo", "multi", "Running"), pod("rtop-demo", "stuck", "Pending")]));
        p
    }

    fn press(p: &mut Picker, code: KeyCode) -> Action {
        p.handle_key(KeyEvent::from(code))
    }

    #[test]
    fn enter_opens_a_running_pod() {
        let mut p = picker();
        assert_eq!(press(&mut p, KeyCode::Enter), Action::Open { ns: "rtop-demo".into(), pod: "multi".into() });
    }

    #[test]
    fn a_pod_that_is_not_running_explains_itself() {
        let mut p = picker();
        press(&mut p, KeyCode::Down);
        assert_eq!(press(&mut p, KeyCode::Enter), Action::None);
        assert!(p.status_text().unwrap().contains("stuck is Pending"));
    }

    #[test]
    fn coming_back_selects_the_pod_just_left() {
        let mut p = picker();
        p.came_back("rtop-demo", "stuck");
        p.set_pods(Ok(vec![pod("rtop-demo", "multi", "Running"), pod("rtop-demo", "stuck", "Pending")]));
        assert_eq!(p.selected_pod().unwrap().name, "stuck");
    }

    #[test]
    fn moving_in_the_namespace_pane_lists_that_namespace() {
        let mut p = picker();
        assert_eq!(p.ns_sel, 3);
        press(&mut p, KeyCode::Tab);
        press(&mut p, KeyCode::Up);
        assert_eq!((p.ns.as_str(), p.pods.is_none()), ("kube-system", true));
        press(&mut p, KeyCode::Home);
        assert_eq!(p.ns, "", "the first entry is all namespaces");
    }

    #[test]
    fn filter_narrows_and_a_typed_namespace_can_be_opened() {
        let mut p = picker();
        for c in "/mul".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        press(&mut p, KeyCode::Enter);
        assert_eq!(p.visible_pods().len(), 1);

        // Listing namespaces was forbidden: type one that is not listed.
        press(&mut p, KeyCode::Tab);
        for c in "/team-a".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        press(&mut p, KeyCode::Enter);
        assert_eq!((p.ns.as_str(), p.focus), ("team-a", Pane::Pods));
    }
}
