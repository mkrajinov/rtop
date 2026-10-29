//! A pod as the target (User Story 1): every container's processes, read by
//! a `sh` reader in each container over `pods/exec`.
//!
//! Containers normally have their own PID namespace, so there is one
//! session per running container and its processes get `space` = the
//! container's index + 1. With `shareProcessNamespace` there is one session
//! and processes are told apart by cgroup (research R4).
//!
//! Collection never blocks the UI: each refresh sends a `snap` request and
//! shows the latest answers that have arrived (research R6).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};

use super::linux_parse as lp;
use super::{Collector, Pending};
use crate::kube::KubeHandle;
use crate::kube::msg::{Event, Incoming, PodInfo, Reply, Request};
use crate::kube::session::{self, Phase, Session};
use crate::model::*;

pub struct PodOptions {
    /// `None`: the kubeconfig's current context.
    pub context: Option<String>,
    /// `None`: the context's namespace.
    pub namespace: Option<String>,
    pub pod: String,
    pub tick: Duration,
}

pub struct KubeCollector {
    k: KubeHandle,
    target: Target,
    implicit_context: bool,
    pod: PodInfo,
    sessions: Vec<Session>,
    /// ExecOpen request id → session index.
    opening: HashMap<u64, usize>,
    tick: Duration,
    /// A target uptime and the local instant it was seen at: turns the
    /// target's clock into `Snapshot::clock`.
    epoch: Option<(f64, Instant)>,
    notes: Vec<String>,
}

/// How long start-up waits for the cluster before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

impl KubeCollector {
    /// Connect, look the pod up and start a reader in every running
    /// container. Errors are the one-line messages of contracts/cli.md.
    pub fn connect(opts: PodOptions) -> anyhow::Result<Self> {
        let mut k = KubeHandle::start();
        let implicit_context = opts.context.is_none();
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
            tick: opts.tick,
            epoch: None,
            notes: Vec::new(),
            pod,
        };
        c.plan_sessions();
        c.first_samples();
        Ok(c)
    }

    /// One session per running container, or one for the whole pod when
    /// its containers share a PID namespace.
    fn plan_sessions(&mut self) {
        self.sessions.clear();
        if self.pod.share_process_namespace {
            if let Some(c) = self.pod.containers.iter().find(|c| c.state == "running") {
                let mut s = Session::new(0, &c.name);
                s.shared = Some(self.container_ids());
                self.sessions.push(s);
            }
        } else {
            for (i, c) in self.pod.containers.iter().enumerate() {
                let mut s = Session::new(i as u16 + 1, &c.name);
                if c.state != "running" {
                    s.stop(&format!("container is {}", c.state));
                }
                self.sessions.push(s);
            }
        }
    }

    fn container_ids(&self) -> HashMap<String, Arc<str>> {
        self.pod.containers.iter().filter_map(|c| Some((c.id.clone()?, Arc::from(c.name.as_str())))).collect()
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
        let Target::Pod { namespace, pod, .. } = &self.target else { return };
        let s = &mut self.sessions[i];
        let container = s.container.to_string();
        let id = s.open(&mut self.k, namespace, pod, &container);
        self.opening.insert(id, i);
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
            Incoming::Event(Event::NodeIndex(_)) => {}
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
        let ids = self.container_ids();
        for i in 0..self.sessions.len() {
            let name = self.sessions[i].container.clone();
            let Some(now) = self.pod.containers.iter().find(|c| *c.name == *name) else { continue };
            let before = old.containers.iter().find(|c| *c.name == *name);
            let restarted = before.is_some_and(|b| b.restarts != now.restarts || b.id != now.id);
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

    /// Reopen sessions that are due and ask the live ones for a sample.
    fn step(&mut self, now: Instant) {
        self.drain();
        for i in 0..self.sessions.len() {
            if self.sessions[i].retry_due(now) {
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

/// Wait for the reply to `id` during start-up (before the TUI runs).
fn wait(k: &KubeHandle, id: u64) -> anyhow::Result<Reply> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match k.recv_timeout(left) {
            Some(Incoming::Reply(got, Reply::Err(e))) if got == id => return Err(anyhow!("{e}")),
            Some(Incoming::Reply(got, r)) if got == id => return Ok(r),
            Some(_) => {}
            None => break,
        }
    }
    bail!("no answer from the API server in {} s", CONNECT_TIMEOUT.as_secs())
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
            return Snapshot { processes, ..Default::default() };
        };
        let r = &sample.resp;
        let text = |n: &str| r.text(n).unwrap_or_default();
        let mut system =
            lp::system(&text("/proc/stat"), &text("/proc/meminfo"), &text("/proc/loadavg"), &text("/proc/uptime"));
        // The pod's network namespace also lists idle tunnel devices
        // (ip_vti0, sit0, ...); only interfaces that moved bytes count.
        let net: Vec<_> = lp::parse_net_dev(&text("/proc/net/dev"))
            .into_iter()
            .filter(|(n, rx, tx)| n != "lo" && rx + tx > 0)
            .collect();
        system.net = Some(IoCounters {
            read: net.iter().map(|n| n.1).sum(),
            write: net.iter().map(|n| n.2).sum(),
            source: format!(
                "{} · /proc/net/dev of the pod",
                net.iter().map(|n| n.0.as_str()).collect::<Vec<_>>().join(" ")
            ),
        });
        system.cpu =
            CpuInfo { model: format!("node {}", self.pod.node.as_deref().unwrap_or("?")), ..Default::default() };
        let up = uptime(&text("/proc/uptime"));
        Snapshot { system, processes, clock: self.clock(up) }
    }

    fn details(&mut self, id: ProcId) -> anyhow::Result<ProcessDetails> {
        self.drain();
        let now = Instant::now();
        let Target::Pod { context, namespace, pod, .. } = &self.target else { bail!("not a pod target") };
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
        // In a shared namespace the process may be in another container
        // than the one the reader runs in.
        let reader = s.container.to_string();
        let container = files
            .cgroup
            .as_deref()
            .and_then(|cg| crate::kube::attribution::container_id(cg))
            .and_then(|cid| self.pod.containers.iter().find(|c| c.id.as_deref() == Some(cid)))
            .map_or(reader.clone(), |c| c.name.clone());
        label(&mut d, &format!("[{pod}/{reader}]"));
        d.notes.push(format!(
            "read over pods/exec in container {reader}. By hand: kubectl --context {context} -n {namespace} exec {pod} -c {reader} -- cat /proc/{}/status",
            id.pid
        ));
        if container != reader {
            d.notes.push(format!("this process belongs to container {container} (from its cgroup)"));
        }
        d.notes.push(format!("'rtop' in the Kernel tab means the reader shell in container {reader}"));
        Ok(d)
    }

    fn target(&self) -> Option<&Target> {
        Some(&self.target)
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
