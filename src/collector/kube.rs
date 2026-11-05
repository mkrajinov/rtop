//! A pod as the target (User Story 1): every container's processes, read by
//! a `sh` reader in each container over `pods/exec`. Or a node (User Story
//! 2): one reader in a `hostPID` helper pod rtop creates on it, with each
//! process attributed to its pod and container by cgroup.
//!
//! Containers normally have their own PID namespace, so there is one
//! session per running container and its processes get `space` = the
//! container's index + 1. With `shareProcessNamespace` there is one session
//! and processes are told apart by cgroup (research R4).
//!
//! Collection never blocks the UI: each refresh sends a `snap` request and
//! shows the latest answers that have arrived (research R6).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};

use super::linux_parse as lp;
use super::{Collector, Pending};
use crate::kube::KubeHandle;
use crate::kube::attribution::Owners;
use crate::kube::msg::{ErrorCode, Event, Incoming, KubeError, NodeIndex, NodeInfo, PodInfo, Reply, Request};
use crate::kube::session::{self, Phase, Session};
use crate::model::*;

pub struct PodOptions {
    /// `None`: the kubeconfig's current context.
    pub context: Option<String>,
    /// The context was not named on the command line: the header warns.
    pub implicit_context: bool,
    /// `None`: the context's namespace.
    pub namespace: Option<String>,
    pub pod: String,
    pub tick: Duration,
    /// Image of the debug container for images without a shell.
    pub debug_image: String,
    /// `--no-debug-container` turns this off: rtop then only reports
    /// containers it cannot read.
    pub debug: bool,
}

pub struct NodeOptions {
    pub context: Option<String>,
    pub implicit_context: bool,
    pub node: String,
    /// Where the helper pod goes; `None`: the context's namespace.
    pub helper_namespace: Option<String>,
    pub tick: Duration,
    pub image: String,
}

/// Set by `main`'s SIGINT/SIGTERM handler: start-up waits give up, and
/// whatever they created is deleted.
pub static CANCEL: AtomicBool = AtomicBool::new(false);

/// Helper pods rtop could not delete on exit, as the commands that delete
/// them. `main` prints these after the terminal is restored.
pub static LEFTOVER: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Node mode: the node, and the helper pod (in `KubeCollector::pod`) whose
/// reader sees all of its processes.
struct NodeMode {
    info: NodeInfo,
    owners: Owners,
    /// Pods on the node, from the index.
    pods: usize,
    image: String,
    deadline: u32,
    /// HelperCreate request id and pod name while a replacement helper is
    /// on its way.
    replacing: Option<(u64, String)>,
}

pub struct KubeCollector {
    k: KubeHandle,
    target: Target,
    implicit_context: bool,
    pod: PodInfo,
    sessions: Vec<Session>,
    /// ExecOpen request id → session index.
    opening: HashMap<u64, usize>,
    /// EnsureDebugContainer request id → session index.
    debugging: HashMap<u64, usize>,
    debug_image: String,
    debug: bool,
    tick: Duration,
    /// A target uptime and the local instant it was seen at: turns the
    /// target's clock into `Snapshot::clock`.
    epoch: Option<(f64, Instant)>,
    notes: Vec<String>,
    node: Option<NodeMode>,
    /// Per container: the last `cpu.stat` and the target uptime it was read
    /// at, and the rates computed from the two samples before it.
    cg_prev: HashMap<String, (lp::CgroupCpuStat, f64)>,
    cg_rates: HashMap<String, (f64, Option<f64>)>,
}

/// A container's own cgroup: with a private cgroup namespace (the
/// default with cgroup v2) the runtime mounts it at /sys/fs/cgroup.
const OWN_CGROUP: &str = "/sys/fs/cgroup";
/// The target container's cgroup seen from its debug container.
const TARGET_CGROUP: &str = "/proc/1/root/sys/fs/cgroup";

/// How long start-up waits for the cluster before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

impl KubeCollector {
    /// Connect, look the pod up and start a reader in every running
    /// container. Errors are the one-line messages of contracts/cli.md.
    pub fn connect(opts: PodOptions) -> anyhow::Result<Self> {
        let mut k = KubeHandle::start();
        let implicit_context = opts.implicit_context || opts.context.is_none();
        let id = k.send(Request::Hello { context: opts.context });
        let Reply::Hello { context, namespace, .. } = wait(&k, id)? else { bail!("unexpected reply to hello") };
        let ns = opts.namespace.unwrap_or(namespace);
        let id = k.send(Request::GetPod { ns: ns.clone(), name: opts.pod.clone() });
        let pod = match wait(&k, id) {
            Ok(Reply::Pod(p)) => p,
            Ok(other) => bail!("unexpected reply {other:?}"),
            Err(e) => bail!("pod {ns}/{}: {e}", opts.pod),
        };
        if pod.phase != "Running" {
            bail!("pod {ns}/{} is {}: {}", pod.name, pod.phase, pod.reason.as_deref().unwrap_or("not running"));
        }
        k.send(Request::WatchPod { ns: ns.clone(), name: pod.name.clone() });
        let target =
            Target::Pod { context, namespace: ns, pod: pod.name.clone(), node: pod.node.clone(), container: None };
        let mut c = KubeCollector {
            k,
            target,
            implicit_context,
            sessions: Vec::new(),
            opening: HashMap::new(),
            debugging: HashMap::new(),
            debug_image: opts.debug_image,
            debug: opts.debug,
            tick: opts.tick,
            epoch: None,
            notes: Vec::new(),
            node: None,
            cg_prev: HashMap::new(),
            cg_rates: HashMap::new(),
            pod,
        };
        c.plan_sessions();
        c.first_samples();
        Ok(c)
    }

    /// Create the helper pod on the node and start a reader in it. Errors
    /// are the one-line messages of contracts/cli.md.
    pub fn connect_node(opts: NodeOptions) -> anyhow::Result<Self> {
        let mut k = KubeHandle::start();
        let implicit_context = opts.implicit_context || opts.context.is_none();
        let id = k.send(Request::Hello { context: opts.context });
        let Reply::Hello { context, namespace, .. } = wait(&k, id)? else { bail!("unexpected reply to hello") };
        let id = k.send(Request::GetNode { name: opts.node.clone() });
        let info = match wait(&k, id) {
            Ok(Reply::Node(n)) => n,
            Ok(other) => bail!("unexpected reply {other:?}"),
            Err(e) => bail!("node {}: {e}", opts.node),
        };
        if !info.ready {
            bail!("node {} is NotReady", info.name);
        }
        let ns = opts.helper_namespace.unwrap_or(namespace);
        // Leftovers of earlier sessions; failing to list them is no reason
        // not to start.
        let id = k.send(Request::HelperGc { ns: ns.clone() });
        let _ = wait(&k, id);
        let deadline = helper_deadline();
        let name = create_helper(&mut k, &context, &info.name, &ns, &opts.image, deadline)?;
        k.send(Request::WatchPod { ns: ns.clone(), name: name.clone() });
        k.send(Request::WatchNodePods { node: info.name.clone() });
        let pod = PodInfo { ns: ns.clone(), name, phase: "Running".into(), ..Default::default() };
        let target = Target::Node { context, node: info.name.clone(), detail: String::new() };
        let mut c = KubeCollector {
            k,
            target,
            implicit_context,
            sessions: Vec::new(),
            opening: HashMap::new(),
            debugging: HashMap::new(),
            debug_image: opts.image.clone(),
            debug: false,
            tick: opts.tick,
            epoch: None,
            notes: Vec::new(),
            node: Some(NodeMode {
                info,
                owners: Owners { unmatched: "host".into(), ..Default::default() },
                pods: 0,
                image: opts.image,
                deadline,
                replacing: None,
            }),
            cg_prev: HashMap::new(),
            cg_rates: HashMap::new(),
            pod,
        };
        c.helper_note();
        c.plan_node_session();
        c.first_samples();
        Ok(c)
    }

    fn helper_note(&mut self) {
        self.notes.retain(|t| !t.starts_with("via helper pod"));
        let text = format!("via helper pod {}/{} (deleted on exit)", self.pod.ns, self.pod.name);
        self.note(&text);
    }

    /// One reader in the helper, pointed at the host's passwd and network
    /// namespace; its own processes are the helper's.
    fn plan_node_session(&mut self) {
        let Some(n) = &self.node else { return };
        let mut s = Session::new(0, "reader");
        s.prelude = "RTOP_ROOT=/proc/1/root\nRTOP_NET=/proc/1/net\n".into();
        let helper = format!("{}/{}", self.pod.ns, self.pod.name);
        s.own = Some(Origin { pod: Some(helper.into()), container: "reader".into() });
        s.shared = Some(n.owners.clone());
        self.sessions = vec![s];
    }

    fn node_index(&mut self, idx: NodeIndex) {
        let Some(n) = &mut self.node else { return };
        let pod = |uid: &str| idx.pods.get(uid).map(|(ns, name)| format!("{ns}/{name}"));
        n.owners.containers = idx
            .containers
            .iter()
            .filter_map(|(id, (uid, c))| {
                Some((id.clone(), Origin { pod: Some(pod(uid)?.into()), container: c.as_str().into() }))
            })
            .collect();
        n.owners.pods = idx.pods.keys().filter_map(|uid| Some((uid.clone(), pod(uid)?.into()))).collect();
        n.pods = idx.pods.values().collect::<std::collections::HashSet<_>>().len();
        for s in &mut self.sessions {
            s.shared = Some(n.owners.clone());
        }
        if let Target::Node { detail, .. } = &mut self.target {
            *detail = node_detail(&n.info, n.pods);
        }
    }

    /// The helper ended (its deadline, or someone deleted it): put a new
    /// one on the node and read from that.
    fn helper_gone(&mut self, why: &str) {
        let Some(n) = &mut self.node else { return };
        if n.replacing.is_some() {
            return;
        }
        let old = self.pod.name.clone();
        // A finished helper stays as Completed until deleted.
        self.k.send(Request::HelperDelete { ns: self.pod.ns.clone(), name: old.clone() });
        let session = session_id();
        let name = crate::kube::helper::name(&n.info.name, &session);
        let id = self.k.send(Request::HelperCreate {
            node: n.info.name.clone(),
            ns: self.pod.ns.clone(),
            image: n.image.clone(),
            deadline_secs: n.deadline,
            session,
        });
        n.replacing = Some((id, name));
        self.close_all();
        for s in &mut self.sessions {
            s.stop(&format!("helper pod {old} {why}; starting a new one"));
        }
    }

    fn helper_ready(&mut self, reply: Reply) {
        let Some(n) = &mut self.node else { return };
        n.replacing = None;
        match reply {
            Reply::Helper { name } => {
                self.k.send(Request::WatchPod { ns: self.pod.ns.clone(), name: name.clone() });
                self.pod.name = name;
                self.helper_note();
                self.plan_node_session();
            }
            Reply::Err(e) => {
                for s in &mut self.sessions {
                    s.stop(&format!("new helper pod: {e}"));
                }
            }
            _ => {}
        }
    }

    /// One session per running container, or one for the whole pod when
    /// its containers share a PID namespace.
    fn plan_sessions(&mut self) {
        self.sessions.clear();
        if self.pod.share_process_namespace {
            if let Some(c) = self.pod.containers.iter().find(|c| c.state == "running") {
                let mut s = Session::new(0, &c.name);
                s.shared = Some(self.owners());
                s.cg_root = Some(OWN_CGROUP.into());
                self.sessions.push(s);
            }
        } else {
            for (i, c) in self.pod.containers.iter().enumerate() {
                let mut s = Session::new(i as u16 + 1, &c.name);
                s.cg_root = Some(OWN_CGROUP.into());
                if c.state != "running" {
                    s.stop(&format!("container is {}", c.state));
                }
                self.sessions.push(s);
            }
        }
    }

    /// The pod's containers by ID, to tell them apart by cgroup.
    fn owners(&self) -> Owners {
        let containers = self
            .pod
            .containers
            .iter()
            .filter_map(|c| Some((c.id.clone()?, Origin { pod: None, container: c.name.as_str().into() })))
            .collect();
        Owners { containers, pods: HashMap::new(), unmatched: "?".into() }
    }

    /// Wait (a few seconds at most) until every session has sent its first
    /// sample, so the first screen is not empty.
    fn first_samples(&mut self) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            self.step(Instant::now());
            if self.sessions.iter().all(|s| s.latest.is_some() || s.phase == Phase::Down) {
                break;
            }
            if let Some(inc) = self.k.recv_timeout(Duration::from_millis(50)) {
                self.route(inc);
            }
        }
    }

    fn open(&mut self, i: usize) {
        let id = self.sessions[i].open(&mut self.k, &self.pod.ns, &self.pod.name);
        self.opening.insert(id, i);
    }

    /// The image has no shell (or no cat): add a debug container that
    /// shares the container's PID namespace and read from there (FR-003).
    fn debug(&mut self, i: usize) {
        let Target::Pod { namespace, pod, .. } = &self.target else { return };
        let s = &mut self.sessions[i];
        if !self.debug {
            s.stop("no sh/cat in the image, and --no-debug-container is set");
            return;
        }
        s.phase = Phase::Starting;
        let id = self.k.send(Request::EnsureDebugContainer {
            ns: namespace.clone(),
            pod: pod.clone(),
            target: s.container.to_string(),
            image: self.debug_image.clone(),
            sleep_secs: 3600,
        });
        self.debugging.insert(id, i);
    }

    fn debug_ready(&mut self, i: usize, reply: Reply) {
        let ns_pod = match &self.target {
            Target::Pod { namespace, pod, .. } => format!("{namespace}/{pod}"),
            Target::Node { node, .. } => node.clone(),
        };
        match reply {
            Reply::Debug { name, created, ptrace } => {
                if !created {
                    self.note(&format!("reading {} through debug container {name}", self.sessions[i].container));
                } else {
                    self.note(&format!(
                        "added debug container {name} ({}) to {ns_pod}; it stays in the pod spec until the pod is deleted",
                        self.debug_image
                    ));
                }
                if !ptrace {
                    self.note(&format!(
                        "{name} runs without SYS_PTRACE (policy): other users' fds and environ are hidden"
                    ));
                }
                let ids = self.owners();
                let s = &mut self.sessions[i];
                s.exec_in = Some(name.into());
                // The target's cgroup, through its PID 1's mount namespace
                // (the debug container shares its PID namespace).
                s.cg_root = Some(TARGET_CGROUP.into());
                // The reader now sits next to the target: tell their
                // processes apart by cgroup, as in a shared namespace.
                s.shared = Some(ids);
                self.open(i);
            }
            Reply::Err(e) => self.sessions[i].stop(&format!("debug container: {e}")),
            other => self.sessions[i].stop(&format!("debug container: unexpected reply {other:?}")),
        }
    }

    /// Everything that arrived since the last call.
    fn drain(&mut self) {
        while let Some(inc) = self.k.try_recv() {
            self.route(inc);
        }
    }

    fn route(&mut self, inc: Incoming) {
        let now = Instant::now();
        match inc {
            Incoming::Reply(id, reply) => {
                if let Some(i) = self.opening.remove(&id) {
                    self.sessions[i].on_open(reply);
                } else if let Some(i) = self.debugging.remove(&id) {
                    self.debug_ready(i, reply);
                } else if self.node.as_ref().is_some_and(|n| n.replacing.as_ref().is_some_and(|r| r.0 == id)) {
                    self.helper_ready(reply);
                }
            }
            Incoming::Event(Event::StreamData { stream, data }) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.stream == Some(stream)) {
                    s.on_data(&data, now);
                }
            }
            Incoming::Event(Event::StreamClosed { stream, exit_code, error }) => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.stream == Some(stream)) {
                    s.on_closed(exit_code, error);
                }
            }
            // The helper reached its deadline: replace it.
            Incoming::Event(Event::Pod(p)) if self.node.is_some() => {
                if p.name == self.pod.name && matches!(p.phase.as_str(), "Succeeded" | "Failed") {
                    self.helper_gone(&format!("is {}", p.phase));
                }
            }
            // Someone else deleted it (an operator, a policy controller):
            // do not put it back.
            Incoming::Event(Event::PodDeleted { name, .. }) if self.node.is_some() => {
                let replacing = self.node.as_ref().is_some_and(|n| n.replacing.is_some());
                if name == self.pod.name && !replacing {
                    self.close_all();
                    for s in &mut self.sessions {
                        s.stop(&format!(
                            "helper pod {name} was deleted outside rtop; Esc and open the node again to start a new one"
                        ));
                    }
                }
            }
            Incoming::Event(Event::Pod(p)) => self.pod_changed(p),
            Incoming::Event(Event::PodDeleted { .. }) => {
                for s in &mut self.sessions {
                    s.stop("pod deleted");
                }
                self.note("pod deleted; rtop reattaches if a pod with this name comes back");
            }
            Incoming::Event(Event::ThreadRestarted { reason }) => {
                for s in &mut self.sessions {
                    s.fail(&format!("kube thread restarted: {reason}"));
                }
            }
            Incoming::Event(Event::NodeIndex(idx)) => self.node_index(idx),
        }
    }

    /// The pod changed: a container restarted, stopped or started, or the
    /// pod was replaced by one with the same name.
    fn pod_changed(&mut self, p: PodInfo) {
        let replaced = p.uid != self.pod.uid;
        let old = std::mem::replace(&mut self.pod, p);
        if let Target::Pod { node, .. } = &mut self.target {
            node.clone_from(&self.pod.node);
        }
        if replaced || old.share_process_namespace != self.pod.share_process_namespace {
            if replaced {
                self.notes.retain(|n| !n.starts_with("pod deleted"));
                self.note("pod was replaced; reattached to the new one");
            }
            self.close_all();
            self.plan_sessions();
            return;
        }
        let ids = self.owners();
        for i in 0..self.sessions.len() {
            let name = self.sessions[i].container.clone();
            let Some(now) = self.pod.containers.iter().find(|c| *c.name == *name) else { continue };
            let before = old.containers.iter().find(|c| *c.name == *name);
            // A container that had no ID yet (still being created) just started.
            let restarted = before.is_some_and(|b| b.restarts != now.restarts || (b.id.is_some() && b.id != now.id));
            let s = &mut self.sessions[i];
            if let Some(shared) = &mut s.shared {
                shared.clone_from(&ids);
            }
            if now.state != "running" {
                if !s.stopped() {
                    s.stop(&format!("container is {}", now.state));
                }
            } else if restarted || s.stopped() {
                // A new container instance: fresh PIDs, fresh reader.
                if let Some(stream) = s.stream {
                    self.k.send(Request::StreamClose { stream });
                }
                let shared = s.shared.take();
                *s = Session::new(s.space, &name);
                s.shared = shared;
                if restarted {
                    self.note(&format!("container {name} restarted ({} restarts)", now.restarts));
                }
            }
        }
    }

    fn close_all(&mut self) {
        for s in &mut self.sessions {
            if let Some(stream) = s.stream.take() {
                self.k.send(Request::StreamClose { stream });
            }
        }
    }

    fn note(&mut self, text: &str) {
        if !self.notes.iter().any(|n| n == text) {
            self.notes.push(text.to_string());
        }
    }

    /// Each container's cgroup usage against its limits, plus requests,
    /// limits and restarts from the pod (User Story 4).
    fn container_usage(&mut self) -> Vec<ContainerUsage> {
        if self.node.is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for c in &self.pod.containers {
            let mut u = ContainerUsage {
                name: c.name.clone(),
                cpu_request: c.requests.get("cpu").cloned(),
                mem_request: c.requests.get("memory").cloned(),
                restarts: c.restarts,
                last_termination: c.last_termination.clone(),
                // Until the cgroup is read, the spec's limits.
                cpu_limit: c.limits.get("cpu").and_then(|q| cpu_quantity(q)),
                mem_max: c.limits.get("memory").and_then(|q| crate::fmt::quantity_bytes(q)),
                unreadable: true,
                ..Default::default()
            };
            // The session whose reader can see this container's cgroup.
            let s = self.sessions.iter().find(|s| *s.container == *c.name && s.cg_root.is_some());
            let sample = s.and_then(|s| s.latest.as_ref());
            let file = |name: &str| sample.and_then(|l| l.resp.text(&format!("cg {name}")));
            u.source = match s {
                None if self.pod.share_process_namespace => {
                    "not read: in a shared PID namespace rtop reads only its own container's cgroup".into()
                }
                None => format!("container is {}", c.state),
                Some(s) => {
                    let via = s.exec_in.as_deref().map(|d| format!(" via {d}")).unwrap_or_default();
                    let root = s.cg_root.as_deref().unwrap_or_default();
                    format!("[{}/{}{via}] {root}/", self.pod.name, c.name)
                }
            };
            if let Some(stat) = file("cpu.stat").map(|t| lp::cgroup_cpu_stat(&t)) {
                u.unreadable = false;
                u.cpu_limit = file("cpu.max").and_then(|t| lp::cgroup_cpu_max(&t));
                u.mem_current = file("memory.current").and_then(|t| lp::cgroup_bytes(&t));
                u.mem_max = file("memory.max").and_then(|t| lp::cgroup_bytes(&t));
                u.mem_peak = file("memory.peak").and_then(|t| lp::cgroup_bytes(&t));
                u.oom_kills = file("memory.events").and_then(|t| lp::cgroup_oom_kills(&t));
                u.throttled_periods = Some(stat.nr_throttled);
                u.throttled_secs = Some(stat.throttled_usec as f64 / 1e6);
                let up = sample.and_then(|l| l.resp.text("/proc/uptime")).and_then(|t| uptime(&t));
                if let Some(up) = up {
                    match self.cg_prev.get(&c.name) {
                        Some((prev, at)) if up > *at && stat.usage_usec >= prev.usage_usec => {
                            let cores = (stat.usage_usec - prev.usage_usec) as f64 / 1e6 / (up - at);
                            let periods = stat.nr_periods.saturating_sub(prev.nr_periods);
                            let thr = (periods > 0).then(|| {
                                stat.nr_throttled.saturating_sub(prev.nr_throttled) as f64 / periods as f64 * 100.0
                            });
                            self.cg_rates.insert(c.name.clone(), (cores, thr));
                            self.cg_prev.insert(c.name.clone(), (stat, up));
                        }
                        // The same sample again: keep the rates.
                        Some((_, at)) if up == *at => {}
                        // First sample, or the container restarted.
                        _ => {
                            self.cg_rates.remove(&c.name);
                            self.cg_prev.insert(c.name.clone(), (stat, up));
                        }
                    }
                }
                if let Some((cores, thr)) = self.cg_rates.get(&c.name) {
                    u.cpu_cores = Some(*cores);
                    u.throttled_pct = *thr;
                }
            } else if let Some(s) = s {
                u.source = match (&s.latest, s.phase, &s.error) {
                    (Some(_), _, _) => format!("{}: cgroup files not readable", u.source.trim_end_matches('/')),
                    (None, Phase::Down, Some(e)) => e.clone(),
                    _ => "connecting…".into(),
                };
            }
            out.push(u);
        }
        out
    }

    /// Reopen sessions that are due and ask the live ones for a sample.
    fn step(&mut self, now: Instant) {
        self.drain();
        for i in 0..self.sessions.len() {
            if let Some(stream) = self.sessions[i].to_close.take() {
                self.k.send(Request::StreamClose { stream });
            }
            if self.sessions[i].needs_debug() && self.sessions[i].phase == Phase::Down {
                self.debug(i);
            } else if self.sessions[i].retry_due(now) {
                self.open(i);
            }
            self.sessions[i].tick(&mut self.k, now, self.tick);
        }
    }

    /// The target's uptime as a local instant, so CPU% divides by the time
    /// between two samples on the target rather than their arrival times.
    fn clock(&mut self, uptime: Option<f64>) -> Option<Instant> {
        let up = uptime?;
        let (base_up, base) = *self.epoch.get_or_insert((up, Instant::now()));
        Some(base + Duration::from_secs_f64((up - base_up).max(0.0)))
    }
}

/// Waiting for a helper includes pulling its image on the node.
const CREATE_TIMEOUT: Duration = Duration::from_secs(75);

/// `RTOP_HELPER_DEADLINE` (seconds) shortens the helper's life, to test
/// what happens when it ends.
fn helper_deadline() -> u32 {
    std::env::var("RTOP_HELPER_DEADLINE").ok().and_then(|v| v.parse().ok()).unwrap_or(3600).max(30)
}

/// A random-enough label value that tells this rtop's helpers apart.
fn session_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{:08x}", (n.as_nanos() as u64 ^ (std::process::id() as u64).wrapping_mul(0x9e37_79b9)) as u32)
}

/// Create the helper and wait until it runs. The name is known before
/// the create is sent, so whatever goes wrong (a timeout, Ctrl-C during an
/// image pull) the pod is deleted rather than left on the node.
fn create_helper(
    k: &mut KubeHandle,
    context: &str,
    node: &str,
    ns: &str,
    image: &str,
    deadline: u32,
) -> anyhow::Result<String> {
    let session = session_id();
    let name = crate::kube::helper::name(node, &session);
    let id = k.send(Request::HelperCreate {
        node: node.into(),
        ns: ns.into(),
        image: image.into(),
        deadline_secs: deadline,
        session,
    });
    let result = wait_cancellable(k, id, CREATE_TIMEOUT);
    match result {
        Ok(Reply::Helper { name }) => return Ok(name),
        // Refused at creation: there is nothing to delete.
        Err(e) if e.downcast_ref::<KubeError>().is_some_and(|e| e.code == ErrorCode::Admission) => {
            bail!(
                "helper pod rejected by admission in {ns}: {e} (pick a namespace that allows hostPID with --helper-namespace)"
            )
        }
        _ => {}
    }
    delete_helper(k, context, ns, &name);
    match result {
        Ok(other) => bail!("unexpected reply {other:?}"),
        Err(e) => bail!("helper pod in {ns}: {e}"),
    }
}

/// `allocatable 8 cpu · 31.2G · 17 pods`
fn node_detail(info: &NodeInfo, pods: usize) -> String {
    let mut parts = Vec::new();
    if let Some(cpu) = info.allocatable.get("cpu") {
        parts.push(format!("{cpu} cpu"));
    }
    if let Some(mem) = info.allocatable.get("memory").and_then(|m| crate::fmt::quantity_bytes(m)) {
        parts.push(crate::fmt::bytes(mem));
    }
    let alloc = if parts.is_empty() { String::new() } else { format!("allocatable {} · ", parts.join(" ")) };
    format!("{alloc}{pods} pods")
}

impl Drop for KubeCollector {
    /// Delete the helper pod, and the replacement on its way if there is
    /// one.
    fn drop(&mut self) {
        let Some(n) = &self.node else { return };
        let context = match &self.target {
            Target::Node { context, .. } | Target::Pod { context, .. } => context.clone(),
        };
        let pending = n.replacing.as_ref().map(|r| r.1.clone());
        let ns = self.pod.ns.clone();
        for name in std::iter::once(self.pod.name.clone()).chain(pending) {
            delete_helper(&mut self.k, &context, &ns, &name);
        }
    }
}

/// Delete a helper pod (already gone counts as deleted). If that fails,
/// the command that deletes it goes to `LEFTOVER` for `main` to print.
fn delete_helper(k: &mut KubeHandle, context: &str, ns: &str, name: &str) {
    let id = k.send(Request::HelperDelete { ns: ns.into(), name: name.into() });
    if !matches!(wait_for(k, id, Duration::from_secs(3)), Ok(Reply::Deleted(_)))
        && let Ok(mut l) = LEFTOVER.lock()
    {
        l.push(format!("kubectl --context {context} -n {ns} delete pod {name}"));
    }
}

/// Wait for the reply to `id` during start-up (before the TUI runs).
fn wait(k: &KubeHandle, id: u64) -> anyhow::Result<Reply> {
    wait_cancellable(k, id, CONNECT_TIMEOUT)
}

/// Like [`wait_for`], but SIGINT/SIGTERM ([`CANCEL`]) ends it early.
fn wait_cancellable(k: &KubeHandle, id: u64, timeout: Duration) -> anyhow::Result<Reply> {
    wait_until(k, id, timeout, true)
}

/// Wait for the reply to `id`; cleanup uses this, so signals do not cut it short.
fn wait_for(k: &KubeHandle, id: u64, timeout: Duration) -> anyhow::Result<Reply> {
    wait_until(k, id, timeout, false)
}

fn wait_until(k: &KubeHandle, id: u64, timeout: Duration, cancellable: bool) -> anyhow::Result<Reply> {
    let deadline = Instant::now() + timeout;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        if cancellable && CANCEL.load(Ordering::Relaxed) {
            bail!("interrupted");
        }
        match k.recv_timeout(left.min(Duration::from_millis(100))) {
            Some(Incoming::Reply(got, Reply::Err(e))) if got == id => return Err(e.into()),
            Some(Incoming::Reply(got, r)) if got == id => return Ok(r),
            _ => {}
        }
    }
    bail!("no answer from the API server in {} s", timeout.as_secs())
}

/// A CPU quantity from the pod spec: `500m`, `2`, `1.5`.
fn cpu_quantity(q: &str) -> Option<f64> {
    match q.strip_suffix('m') {
        Some(m) => m.parse::<f64>().ok().map(|m| m / 1000.0),
        None => q.parse().ok(),
    }
}

fn uptime(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

impl Collector for KubeCollector {
    fn platform(&self) -> &'static str {
        "Kubernetes"
    }

    fn snapshot(&mut self) -> Snapshot {
        self.step(Instant::now());
        let Self { k, sessions, .. } = self;
        let mut processes = Vec::new();
        for s in sessions.iter_mut() {
            processes.extend(s.rows(k));
        }
        // /proc/stat, meminfo, ... show the whole node in every container:
        // take them from one session instead of adding them up.
        let primary = self.sessions.iter().filter_map(|s| s.latest.as_ref()).max_by_key(|l| l.at);
        let Some(sample) = primary else {
            // Nothing read yet (or the container is restarting): the
            // containers card still shows the spec and restarts.
            return Snapshot { processes, containers: self.container_usage(), ..Default::default() };
        };
        let r = &sample.resp;
        let text = |n: &str| r.text(n).unwrap_or_default();
        let mut system =
            lp::system(&text("/proc/stat"), &text("/proc/meminfo"), &text("/proc/loadavg"), &text("/proc/uptime"));
        let page_size = self.sessions.iter().find_map(|s| s.env.map(|e| e.page_size)).unwrap_or(4096);
        system.swap_io = lp::vmstat_swap(&text("/proc/vmstat"), page_size, "node /proc/vmstat pswpin, pswpout");
        system.pressure = lp::psi(&text("/proc/pressure/memory"));
        // A network namespace also lists idle tunnel devices (ip_vti0,
        // sit0, ...); only interfaces that moved bytes count. On a node the
        // reader reads /proc/1/net/dev: the host's namespace, not the
        // helper pod's.
        let net: Vec<_> = lp::parse_net_dev(&text("/proc/net/dev"))
            .into_iter()
            .filter(|(n, rx, tx)| n != "lo" && rx + tx > 0)
            .collect();
        system.net = Some(IoCounters {
            read: net.iter().map(|n| n.1).sum(),
            write: net.iter().map(|n| n.2).sum(),
            source: format!(
                "{} · {}",
                net.iter().map(|n| n.0.as_str()).collect::<Vec<_>>().join(" "),
                if self.node.is_some() { "/proc/1/net/dev of the node" } else { "/proc/net/dev of the pod" }
            ),
        });
        let node = match &self.node {
            Some(n) => n.info.name.as_str(),
            None => self.pod.node.as_deref().unwrap_or("?"),
        };
        system.cpu = CpuInfo { model: format!("node {node}"), ..Default::default() };
        let up = uptime(&text("/proc/uptime"));
        let containers = self.container_usage();
        Snapshot { system, processes, clock: self.clock(up), containers }
    }

    fn details(&mut self, id: ProcId) -> anyhow::Result<ProcessDetails> {
        self.drain();
        let now = Instant::now();
        let s = self
            .sessions
            .iter_mut()
            .find(|s| s.space == id.space)
            .ok_or_else(|| anyhow!("no reader for this process"))?;
        let fresh = s.answers.get(&id.pid).is_some_and(|(t, _)| now - *t < self.tick);
        if !fresh && !s.details_pending(id.pid) {
            s.request_details(&mut self.k, id.pid, now);
        }
        let Some((_, resp)) = s.answers.get(&id.pid) else {
            if s.phase == Phase::Down {
                bail!("container {}: {}", s.container, s.error.as_deref().unwrap_or("not connected"));
            }
            return Err(Pending.into());
        };
        if resp.get("stat").is_none() {
            bail!("/proc/{}/stat is gone: the process exited", id.pid);
        }
        let env = s.env.ok_or_else(|| anyhow!("no sample yet"))?;
        let files = session::proc_files(id.pid, resp);
        let users = &s.users;
        let mut d = lp::details(&files, &env, &mut |uid| users.get(&uid).cloned().unwrap_or(uid.to_string()))?;
        let (ns, helper) = (&self.pod.ns, &self.pod.name);
        if let (Target::Node { context, node, .. }, Some(n)) = (&self.target, &self.node) {
            label(&mut d, &format!("[node {node}]"));
            let own = s.own.clone().unwrap_or(Origin { pod: None, container: "reader".into() });
            let owner = n.owners.owner(files.cgroup.as_deref().unwrap_or_default(), &d.name, &own);
            d.notes.push(match &owner.pod {
                Some(pod) => format!("belongs to pod {pod}, container {} (from its cgroup)", owner.container),
                None => format!("belongs to {} (its cgroup matches no pod on the node)", owner.container),
            });
            d.notes.push(format!(
                "read over pods/exec in helper pod {ns}/{helper}, which shares the node's PID namespace (hostPID). By hand: kubectl --context {context} -n {ns} exec {helper} -- cat /proc/{}/status",
                id.pid
            ));
            d.notes.push("'rtop' in the Kernel tab means the reader shell in the helper pod".into());
            return Ok(d);
        }
        let Target::Pod { context, namespace, pod, .. } = &self.target else { bail!("not a pod target") };
        // Where the reader runs: the container itself, or its debug
        // container. In a shared namespace (or next to a debug container)
        // the process itself may be in another container.
        let target = s.container.to_string();
        let exec_in = s.exec_in.as_deref().unwrap_or(&target).to_string();
        let container = files
            .cgroup
            .as_deref()
            .and_then(|cg| crate::kube::attribution::container_id(cg))
            .and_then(|cid| self.pod.containers.iter().find(|c| c.id.as_deref() == Some(cid)))
            .map_or(target.clone(), |c| c.name.clone());
        let via = if exec_in == target { String::new() } else { format!(" via debug container {exec_in}") };
        label(&mut d, &format!("[{pod}/{target}{via}]"));
        d.notes.push(format!(
            "read over pods/exec in container {exec_in}. By hand: kubectl --context {context} -n {namespace} exec {pod} -c {exec_in} -- cat /proc/{}/status",
            id.pid
        ));
        if container != target {
            d.notes.push(format!("this process belongs to container {container} (from its cgroup)"));
        }
        d.notes.push(format!("'rtop' in the Kernel tab means the reader shell in container {exec_in}"));
        Ok(d)
    }

    fn target(&self) -> Option<&Target> {
        Some(&self.target)
    }

    fn signal(&mut self, id: ProcId, signal: &str) -> anyhow::Result<()> {
        if self.node.is_some() {
            bail!("node mode is read-only: signals are sent only inside a pod (rtop k8s --pod ... --allow-actions)");
        }
        let s = self.sessions.iter_mut().find(|s| s.space == id.space).ok_or_else(|| anyhow!("no reader"))?;
        if !s.signal(&mut self.k, signal, id.pid) {
            bail!("container {} is not connected", s.container);
        }
        Ok(())
    }

    fn messages(&mut self) -> Vec<String> {
        self.drain();
        self.sessions.iter_mut().flat_map(|s| s.messages.drain(..)).collect()
    }

    fn link(&self) -> Option<Link> {
        let now = Instant::now();
        let stale = self
            .sessions
            .iter()
            .filter(|s| s.phase != Phase::Live)
            .filter_map(|s| s.latest.as_ref().map(|l| now - l.at))
            .filter(|age| *age > self.tick * 2)
            .max();
        let mut notes = self.notes.clone();
        for s in &self.sessions {
            if let (Phase::Down, Some(e)) = (s.phase, &s.error) {
                notes.push(format!("{}: {e}", s.container));
            }
        }
        Some(Link { implicit_context: self.implicit_context, stale, notes })
    }
}

/// Say where every value was read: `[pod/container] /proc/7/status VmRSS`.
fn label(d: &mut ProcessDetails, prefix: &str) {
    let fields = d
        .overview
        .iter_mut()
        .chain(d.memory.iter_mut())
        .chain(d.limits.iter_mut())
        .chain(d.kernel.sections.iter_mut().flat_map(|(_, f)| f.iter_mut()));
    for f in fields {
        f.source = format!("{prefix} {}", f.source);
    }
    for r in &mut d.raw {
        r.title = format!("{prefix} {}", r.title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/kube/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// T034: answers captured from busybox in the kind cluster go through the
    /// same parsers as local /proc.
    #[test]
    fn kind_capture_parses_like_local_proc() {
        let (mut k, _api) = crate::kube::client::tests::mocked();
        let mut s = Session::new(2, "sidecar");
        s.phase = Phase::Live;
        s.on_data(&fixture("kind-hello.bin"), Instant::now());
        assert_eq!(s.phase, Phase::Live);
        s.stream = Some(1);
        s.tick(&mut k, Instant::now(), Duration::from_secs(2));
        s.on_data(&fixture("kind-snap.bin"), Instant::now());
        let rows = s.rows(&mut k);
        assert!(rows.len() >= 2, "{rows:?}");
        let init = rows.iter().find(|p| p.pid == 1).unwrap();
        assert_eq!((init.user.as_str(), init.space), ("root", 2));
        assert!(init.rss > 0 && init.start_time > 1_600_000_000);
        // The reader labels itself.
        assert!(rows.iter().any(|p| p.cmdline.contains("rtop's reader")), "{rows:?}");

        let mut d = crate::kube::wire::Decoder::default();
        d.push(&fixture("kind-details.bin"));
        let resp = d.next_response().unwrap();
        let files = session::proc_files(1, &resp);
        assert!(files.fds.as_ref().is_some_and(|f| !f.is_empty()));
        assert!(!files.ns.is_empty() && !files.tasks.is_empty());
        let env = s.env.unwrap();
        let mut det = lp::details(&files, &env, &mut |u| u.to_string()).unwrap();
        label(&mut det, "[multi/sidecar]");
        assert!(det.overview.iter().all(|f| f.source.starts_with("[multi/sidecar] ")));
        assert!(!det.limits.is_empty() && !det.regions.is_empty());
    }

    /// A node-mode collector reading through helper `rtop-demo/rtop-probe`.
    fn node_collector(k: KubeHandle) -> KubeCollector {
        KubeCollector {
            k,
            target: Target::Node {
                context: "kind-rtop".into(),
                node: "rtop-control-plane".into(),
                detail: String::new(),
            },
            implicit_context: false,
            pod: PodInfo { ns: "rtop-demo".into(), name: "rtop-probe".into(), ..Default::default() },
            sessions: Vec::new(),
            opening: HashMap::new(),
            debugging: HashMap::new(),
            debug_image: String::new(),
            debug: false,
            tick: Duration::from_secs(2),
            epoch: None,
            notes: Vec::new(),
            node: Some(NodeMode {
                info: NodeInfo { name: "rtop-control-plane".into(), ready: true, ..Default::default() },
                owners: Owners { unmatched: "host".into(), ..Default::default() },
                pods: 0,
                image: String::new(),
                deadline: 3600,
                replacing: None,
            }),
            cg_prev: HashMap::new(),
            cg_rates: HashMap::new(),
        }
    }

    /// A helper that cannot be deleted is reported with the command that
    /// deletes it, for `main` to print after the TUI closes.
    #[test]
    fn undeletable_helper_is_reported() {
        let (k, mut api) = crate::kube::client::tests::mocked();
        let mut c = node_collector(k);
        c.pod.name = "rtop-probe-403".into();
        let answer = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let status = serde_json::json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "code": 403,
                "reason": "Forbidden", "message": "pods \"rtop-probe\" is forbidden: User \"dev\" cannot delete resource \"pods\""});
            let path = rt.block_on(crate::kube::client::tests::answer(&mut api, 403, status));
            assert_eq!(path, "/api/v1/namespaces/rtop-demo/pods/rtop-probe-403");
        });
        drop(c);
        answer.join().unwrap();
        let left = LEFTOVER.lock().unwrap().clone();
        assert!(left.iter().any(|l| l.ends_with("-n rtop-demo delete pod rtop-probe-403")), "{left:?}");
    }

    /// T036/T044: a reader in a hostPID helper on kind. Processes of pods
    /// are attributed through the node index, the rest is `host`.
    #[test]
    fn node_capture_attributes_processes() {
        let path = format!("{}/tests/fixtures/kube/pods-on-node.json", env!("CARGO_MANIFEST_DIR"));
        let list: k8s_openapi::List<k8s_openapi::api::core::v1::Pod> =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let idx = crate::kube::convert::node_index(&list.items);
        let mut c = node_collector(crate::kube::client::tests::mocked().0);
        c.plan_node_session();
        c.node_index(idx);
        assert!(matches!(&c.target, Target::Node { detail, .. } if detail.ends_with("13 pods")));
        let bytes = fixture("kind-node.bin");
        let cut = bytes.windows(5).position(|w| w == b"\x1eD 0\n").unwrap() + 5;
        let s = &mut c.sessions[0];
        s.phase = Phase::Live;
        s.stream = Some(1);
        s.on_data(&bytes[..cut], Instant::now());
        // The host's /etc/passwd, through /proc/1/root.
        assert_eq!(s.users.get(&1).map(String::as_str), Some("daemon"));
        s.tick(&mut c.k, Instant::now(), Duration::from_secs(2));
        let rest = &bytes[cut..];
        let cmd_at = rest.windows(5).position(|w| w == b"\x1eD 1\n").unwrap() + 5;
        s.on_data(&rest[..cmd_at], Instant::now());
        assert_eq!(s.rows(&mut c.k).len(), 54);
        s.on_data(&rest[cmd_at..], Instant::now());
        let rows = s.rows(&mut c.k);
        let by_pid = |pid| rows.iter().find(|p| p.pid == pid).unwrap().origin.clone().unwrap();
        let etcd = by_pid(579);
        assert_eq!(
            (etcd.pod.as_deref(), etcd.container.as_ref()),
            (Some("kube-system/etcd-rtop-control-plane"), "etcd")
        );
        for host in [1, 130, 744] {
            assert_eq!(by_pid(host), Origin { pod: None, container: "host".into() }, "pid {host}");
        }
        let snap = c.snapshot();
        assert!(snap.system.net.unwrap().source.contains("/proc/1/net/dev of the node"));
        // Nothing to delete: there is no API server behind this one.
        c.node = None;
    }

    /// T020: two containers each have a PID 1 and a PID 7; merged, they stay
    /// distinct processes and parents resolve inside each container.
    #[test]
    fn two_sessions_merge_into_distinct_ids() {
        let snap = std::fs::read(format!("{}/tests/fixtures/kube/snap-pod.bin", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let (mut k, _api) = crate::kube::client::tests::mocked();
        let mut all = Vec::new();
        for (space, name) in [(1, "app"), (2, "sidecar")] {
            let mut s = Session::new(space, name);
            s.phase = Phase::Live;
            s.on_data(b"\x1eF passwd\nroot:x:0:0::/:/bin/sh\n\x1eE 0\n\x1eD 0\n", Instant::now());
            // The session only takes answers it asked for: pretend it did.
            s.on_data(&snap, Instant::now());
            assert!(s.latest.is_none(), "unrequested answers are dropped");
            s.stream = Some(space as u32);
            s.tick(&mut k, Instant::now(), Duration::from_secs(2));
            s.on_data(&snap, Instant::now());
            all.extend(s.rows(&mut k));
        }
        assert_eq!(all.len(), 4);
        let ids: std::collections::HashSet<_> = all.iter().map(Process::id).collect();
        assert_eq!(ids.len(), 4);
        let worker = all.iter().find(|p| p.space == 2 && p.pid == 7).unwrap();
        assert_eq!(worker.parent_id(), ProcId { space: 2, pid: 1 });
        assert_eq!(worker.origin.as_ref().unwrap().container.as_ref(), "sidecar");
        assert_eq!(all.iter().find(|p| p.pid == 1).unwrap().user, "root");
    }
}
