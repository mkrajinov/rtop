//! The synchronous side of one reader session: a `sh` running
//! [`READER`] in one container (or debug container, or helper pod) and the
//! answers it has sent back (contracts/reader-wire.md).
//!
//! Nothing here blocks. Requests go out through the [`KubeHandle`]; the
//! collector routes the matching replies and stream bytes back in, and
//! takes whatever whole answers have arrived on each tick.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::KubeHandle;
use super::attribution;
use super::msg::{KubeError, Reply, Request, StreamId};
use super::wire::{self, Decoder, Response};
use crate::collector::linux_parse::{self as lp, LinuxEnv, ProcFiles};
use crate::model::{Origin, Pid, Process};

/// The script `sh` runs inside the target.
pub const READER: &str = include_str!("reader.sh");

/// `USER_HZ`: the unit of every tick count in /proc. Fixed at 100 in the
/// Linux user ABI, whatever the kernel's internal HZ.
const TICKS: u64 = 100;

const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Exec requested, or running but the hello has not arrived yet.
    Starting,
    Live,
    /// Live, but the last snap is overdue.
    Stale,
    /// Ended or failed; reopened after `retry_at`.
    Down,
}

/// The newest complete `snap` answer and when it arrived.
pub struct Sample {
    pub resp: Response,
    pub at: Instant,
}

pub struct Session {
    pub space: u16,
    pub container: Arc<str>,
    pub phase: Phase,
    pub stream: Option<StreamId>,
    decoder: Decoder,
    next_seq: u64,
    page_size: Option<u64>,
    /// PID of the reader shell itself, labelled in the table.
    reader_pid: Option<Pid>,
    pub env: Option<LinuxEnv>,
    pub users: HashMap<u32, String>,
    snap_sent: Option<(u64, Instant)>,
    pub latest: Option<Sample>,
    cmd_sent: Option<(u64, Vec<(Pid, u64)>)>,
    /// argv and cgroup by (pid, start time): fetched once per process,
    /// since they rarely change and argv is the biggest file of a snapshot.
    cmdlines: HashMap<(Pid, u64), (Vec<u8>, String)>,
    /// Pod with one shared PID namespace: container ID → name, to tell the
    /// containers apart by cgroup. `None`: every process is `container`'s.
    pub shared: Option<HashMap<String, Arc<str>>>,
    details_sent: HashMap<u64, (Pid, Instant)>,
    /// Last `details` answer per PID.
    pub answers: HashMap<Pid, (Instant, Response)>,
    backoff: Duration,
    retry_at: Option<Instant>,
    pub error: Option<String>,
}

impl Session {
    pub fn new(space: u16, container: &str) -> Self {
        Session {
            space,
            container: container.into(),
            phase: Phase::Down,
            stream: None,
            decoder: Decoder::default(),
            next_seq: 1,
            page_size: None,
            reader_pid: None,
            env: None,
            users: HashMap::new(),
            snap_sent: None,
            latest: None,
            cmd_sent: None,
            cmdlines: HashMap::new(),
            shared: None,
            details_sent: HashMap::new(),
            answers: HashMap::new(),
            backoff: Duration::from_secs(1),
            retry_at: Some(Instant::now()),
            error: None,
        }
    }

    /// Start `sh` with the reader in `container` (which may be a debug
    /// container next to the one being read). Returns the request id whose
    /// reply carries the stream.
    pub fn open(&mut self, k: &mut KubeHandle, ns: &str, pod: &str, container: &str) -> u64 {
        self.phase = Phase::Starting;
        self.retry_at = None;
        self.stream = None;
        self.decoder = Decoder::default();
        self.snap_sent = None;
        self.cmd_sent = None;
        self.details_sent.clear();
        k.send(Request::ExecOpen {
            ns: ns.into(),
            pod: pod.into(),
            container: container.into(),
            command: vec!["sh".into()],
            stdin_init: READER.as_bytes().to_vec(),
        })
    }

    pub fn on_open(&mut self, reply: Reply) {
        match reply {
            Reply::Stream(id) => self.stream = Some(id),
            Reply::Err(e) => self.fail(&e.to_string()),
            other => self.fail(&format!("unexpected reply {other:?}")),
        }
    }

    pub fn on_closed(&mut self, exit_code: Option<i32>, error: Option<KubeError>) {
        let why = match (error, exit_code) {
            (Some(e), _) => e.to_string(),
            (None, Some(127)) => "sh: not found in the image".into(),
            (None, Some(c)) => format!("reader exited with status {c}"),
            (None, None) => "session closed".into(),
        };
        self.fail(&why);
    }

    /// Down until the next retry, 1 s after the first failure and doubling
    /// up to 30 s. The last sample stays on screen, marked stale.
    pub fn fail(&mut self, why: &str) {
        self.phase = Phase::Down;
        self.stream = None;
        self.error = Some(why.to_string());
        self.retry_at = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    /// Stay down: the container is gone or cannot be read.
    pub fn stop(&mut self, why: &str) {
        self.fail(why);
        self.retry_at = None;
    }

    /// Down with no retry planned (see [`Session::stop`]).
    pub fn stopped(&self) -> bool {
        self.phase == Phase::Down && self.retry_at.is_none()
    }

    pub fn retry_due(&self, now: Instant) -> bool {
        self.phase == Phase::Down && self.retry_at.is_some_and(|t| now >= t)
    }

    pub fn on_data(&mut self, data: &[u8], now: Instant) {
        self.decoder.push(data);
        while let Some(resp) = self.decoder.next_response() {
            self.answer(resp, now);
        }
    }

    fn answer(&mut self, resp: Response, now: Instant) {
        if resp.seq == 0 {
            self.hello(&resp);
            self.phase = Phase::Live;
            self.backoff = Duration::from_secs(1);
            self.error = None;
            return;
        }
        if self.snap_sent.is_some_and(|(s, _)| s == resp.seq) {
            self.snap_sent = None;
            if self.env.is_none() {
                let btime = resp.text("/proc/stat").map_or(0, |s| lp::proc_stat(&s).1);
                self.env = Some(LinuxEnv { ticks: TICKS, page_size: self.page_size.unwrap_or(4096), btime });
            }
            if self.phase == Phase::Stale {
                self.phase = Phase::Live;
            }
            self.latest = Some(Sample { resp, at: now });
        } else if let Some((_, keys)) = self.cmd_sent.take_if(|(s, _)| *s == resp.seq) {
            for (pid, start) in keys {
                let raw = resp.get(&format!("/proc/{pid}/cmdline")).map(|f| f.body.clone()).unwrap_or_default();
                let cgroup = resp.text(&format!("/proc/{pid}/cgroup")).unwrap_or_default();
                self.cmdlines.insert((pid, start), (raw, cgroup));
            }
        } else if let Some((pid, _)) = self.details_sent.remove(&resp.seq) {
            self.answers.insert(pid, (now, resp));
        }
        // Anything else answers a request that already timed out.
    }

    fn hello(&mut self, r: &Response) {
        self.users = r.text("passwd").map(|p| passwd(&p)).unwrap_or_default();
        self.reader_pid = r.text("pid").and_then(|s| s.trim().parse().ok());
        self.page_size = r.text("pagesize").and_then(|s| s.trim().parse().ok()).or_else(|| {
            // No getconf: resident pages in statm against VmRSS in kB.
            page_size(&r.text("statm")?, &r.text("status")?)
        });
    }

    /// Send `<verb> <seq> <args>`; the answer will carry the same seq.
    fn write(&mut self, k: &mut KubeHandle, verb: &str, args: &str) -> Option<u64> {
        let stream = self.stream?;
        let seq = self.next_seq;
        self.next_seq += 1;
        k.send(Request::StreamWrite { stream, data: format!("{verb} {seq} {args}\n").into_bytes() });
        Some(seq)
    }

    /// Once per refresh: ask for a new snapshot unless one is on its way.
    /// One that takes longer than two refreshes marks the session stale;
    /// after 30 s the session is given up and reopened.
    pub fn tick(&mut self, k: &mut KubeHandle, now: Instant, every: Duration) {
        if !matches!(self.phase, Phase::Live | Phase::Stale) {
            return;
        }
        match self.snap_sent {
            Some((_, sent)) if now - sent > MAX_BACKOFF => {
                if let Some(stream) = self.stream {
                    k.send(Request::StreamClose { stream });
                }
                self.fail("no answer for 30 s");
            }
            Some((_, sent)) if now - sent > every * 2 => self.phase = Phase::Stale,
            Some(_) => {}
            None => {
                if let Some(seq) = self.write(k, "snap", "") {
                    self.snap_sent = Some((seq, now));
                }
            }
        }
    }

    /// Ask for the details of `pid` unless that is already under way.
    pub fn request_details(&mut self, k: &mut KubeHandle, pid: Pid, now: Instant) {
        let busy = self.details_sent.values().any(|&(p, sent)| p == pid && now - sent < Duration::from_secs(5));
        if busy || !matches!(self.phase, Phase::Live | Phase::Stale) {
            return;
        }
        self.details_sent.retain(|_, (_, sent)| now - *sent < Duration::from_secs(5));
        if let Some(seq) = self.write(k, "details", &pid.to_string()) {
            self.details_sent.insert(seq, (pid, now));
        }
    }

    pub fn details_pending(&self, pid: Pid) -> bool {
        self.details_sent.values().any(|&(p, _)| p == pid)
    }

    /// The rows of the latest sample. Processes whose argv is not known yet
    /// show their name until the `cmd` answer arrives.
    pub fn rows(&mut self, k: &mut KubeHandle) -> Vec<Process> {
        let (Some(sample), Some(env)) = (&self.latest, &self.env) else { return Vec::new() };
        let origin = Origin { pod: None, container: self.container.clone() };
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for r in sample.resp.procs() {
            let user = r.uid.map_or_else(|| "?".to_string(), |u| self.users.get(&u).cloned().unwrap_or(u.to_string()));
            let Ok(mut p) = lp::process_row(&r.stat, r.uid.unwrap_or(u32::MAX), user, b"", r.fds, env) else {
                continue;
            };
            p.space = self.space;
            p.origin = Some(origin.clone());
            match self.cmdlines.get(&(p.pid, p.start_time)) {
                Some((raw, cgroup)) => {
                    if !p.cmdline.is_empty() {
                        p.cmdline = lp::cmdline(raw, &p.name);
                    }
                    if let Some(ids) = &self.shared {
                        let c = shared_container(ids, cgroup, &p.name, &self.container);
                        p.origin = Some(Origin { pod: None, container: c });
                    }
                }
                None => {
                    if !p.cmdline.is_empty() {
                        p.cmdline = p.name.clone();
                    }
                    if self.shared.is_some() {
                        p.origin = None;
                    }
                    missing.push((p.pid, p.start_time));
                }
            }
            if Some(p.pid) == self.reader_pid {
                p.cmdline = "sh  ← rtop's reader (pods/exec)".into();
            }
            out.push(p);
        }
        if self.shared.is_some() {
            // Not looked up yet: a new process is almost always in its
            // parent's container.
            let known: HashMap<Pid, Origin> = out.iter().filter_map(|p| Some((p.pid, p.origin.clone()?))).collect();
            for p in out.iter_mut().filter(|p| p.origin.is_none()) {
                p.origin = Some(known.get(&p.ppid).cloned().unwrap_or(Origin { pod: None, container: "?".into() }));
            }
        }
        let live: std::collections::HashSet<_> = out.iter().map(|p| (p.pid, p.start_time)).collect();
        self.cmdlines.retain(|key, _| live.contains(key));
        if !missing.is_empty() && self.cmd_sent.is_none() {
            missing.truncate(200);
            let pids: Vec<String> = missing.iter().map(|(p, _)| p.to_string()).collect();
            if let Some(seq) = self.write(k, "cmd", &pids.join(" ")) {
                self.cmd_sent = Some((seq, missing));
            }
        }
        out
    }
}

/// Which container of a shared-namespace pod a process is in. The reader
/// sits in a cgroup namespace, so its own container's cgroup reads as `/`
/// and the others as `/../cri-containerd-<id>.scope`. The pause container
/// holds the namespace and is not in the pod status.
fn shared_container(ids: &HashMap<String, Arc<str>>, cgroup: &str, name: &str, own: &Arc<str>) -> Arc<str> {
    if cgroup.lines().any(|l| l.trim_end() == "0::/") {
        return own.clone();
    }
    match attribution::container_id(cgroup).and_then(|id| ids.get(id)) {
        Some(c) => c.clone(),
        None if name == "pause" => "pause".into(),
        None => "?".into(),
    }
}

/// uid → name from /etc/passwd.
pub fn passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split(':');
            let name = it.next()?;
            Some((it.nth(1)?.parse().ok()?, name.to_string()))
        })
        .collect()
}

/// Page size from one process's resident pages (statm field 2) and its
/// VmRSS in kB, rounded to the sizes Linux uses (4, 16 or 64 KiB).
pub fn page_size(statm: &str, status: &str) -> Option<u64> {
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let kb: u64 = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.split_whitespace().next()?.parse().ok()?;
    let raw = (kb * 1024).checked_div(pages)?;
    [4096u64, 16384, 65536].into_iter().min_by_key(|s| s.abs_diff(raw))
}

/// The files of a `details` answer in the shape the Linux parser takes.
pub fn proc_files(pid: Pid, r: &Response) -> ProcFiles {
    let text = |n: &str| r.text(n);
    let mut sockets = HashMap::new();
    for (file, proto, v6) in
        [("tcp", "TCP", false), ("tcp6", "TCP6", true), ("udp", "UDP", false), ("udp6", "UDP6", true)]
    {
        if let Some(t) = text(&format!("net/{file}")) {
            sockets.extend(lp::parse_net(&t, proto, v6));
        }
    }
    if let Some(t) = text("net/unix") {
        sockets.extend(lp::parse_unix(&t));
    }
    let mut tasks: Vec<(u64, String)> = r
        .frames
        .iter()
        .filter(|f| f.ok())
        .filter_map(|f| Some((f.name.strip_prefix("t ")?.parse().ok()?, f.text())))
        .collect();
    tasks.sort();
    let cgroup_files = r
        .frames
        .iter()
        .filter(|f| f.ok())
        .filter_map(|f| Some((f.name.strip_prefix("cg ")?.to_string(), f.text())))
        .collect();
    ProcFiles {
        pid,
        stat: text("stat").unwrap_or_default(),
        status: text("status").unwrap_or_default(),
        cmdline: r.get("cmdline").map(|f| f.body.clone()).unwrap_or_default(),
        environ: r.get("environ").map(|f| f.body.clone()),
        io: text("io"),
        limits: text("limits"),
        cgroup: text("cgroup"),
        cgroup_files,
        schedstat: text("schedstat"),
        smaps_rollup: text("smaps_rollup"),
        maps: text("maps"),
        wchan: text("wchan"),
        oom_score: text("oom_score"),
        oom_score_adj: text("oom_score_adj"),
        exe: text("exe").map(|s| s.trim_end().to_string()),
        cwd: text("cwd").map(|s| s.trim_end().to_string()),
        ns: text("ns").map(|s| wire::named_links(&s)).unwrap_or_default(),
        self_ns: text("self_ns").map(|s| wire::named_links(&s)).unwrap_or_default(),
        fds: text("fd").map(|s| wire::links(&s)),
        tasks,
        sockets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd_maps_uids() {
        let u = passwd("root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534::/:/sbin/nologin\nbroken\n");
        assert_eq!(u.get(&0).map(String::as_str), Some("root"));
        assert_eq!(u.get(&65534).map(String::as_str), Some("nobody"));
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn page_size_from_statm_and_vmrss() {
        let status = "Name:\tsh\nVmRSS:\t    1788 kB\n";
        assert_eq!(page_size("1000 447 300 1 0 100 0", status), Some(4096));
        assert_eq!(page_size("1000 112 300 1 0 100 0", status), Some(16384));
        assert_eq!(page_size("1000 0 0 0 0 0 0", status), None);
    }
}
