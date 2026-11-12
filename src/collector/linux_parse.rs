//! Turns the text of /proc files into the rtop model.
//!
//! This module never touches the filesystem, so it compiles and is tested on
//! every platform against captured fixtures; `linux.rs` does the reading.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use anyhow::Context;
use procfs_core::FromRead;
use procfs_core::process::{SmapsRollup, Stat, Status};

use crate::fmt;
use crate::model::*;

/// Constants the kernel does not put in each file.
#[derive(Clone, Copy, Debug)]
pub struct LinuxEnv {
    /// `sysconf(_SC_CLK_TCK)`, the unit of utime/stime/starttime (usually 100).
    pub ticks: u64,
    pub page_size: u64,
    /// `btime` from /proc/stat: boot time as a Unix timestamp.
    pub btime: u64,
}

/// Field names of /proc/<pid>/stat in order (see proc_pid_stat(5)).
pub const STAT_FIELDS: [&str; 52] = [
    "pid",
    "comm",
    "state",
    "ppid",
    "pgrp",
    "session",
    "tty_nr",
    "tpgid",
    "flags",
    "minflt",
    "cminflt",
    "majflt",
    "cmajflt",
    "utime",
    "stime",
    "cutime",
    "cstime",
    "priority",
    "nice",
    "num_threads",
    "itrealvalue",
    "starttime",
    "vsize",
    "rss",
    "rsslim",
    "startcode",
    "endcode",
    "startstack",
    "kstkesp",
    "kstkeip",
    "signal",
    "blocked",
    "sigignore",
    "sigcatch",
    "wchan",
    "nswap",
    "cnswap",
    "exit_signal",
    "processor",
    "rt_priority",
    "policy",
    "delayacct_blkio_ticks",
    "guest_time",
    "cguest_time",
    "start_data",
    "end_data",
    "start_brk",
    "arg_start",
    "arg_end",
    "env_start",
    "env_end",
    "exit_code",
];

fn stat_hint(name: &str) -> String {
    format!(" {name} ")
}

pub fn parse_stat(text: &str) -> anyhow::Result<Stat> {
    Stat::from_read(text.as_bytes()).context("parsing /proc/<pid>/stat")
}

/// `\0`-separated argv to a display string. Kernel threads have none.
pub fn cmdline(raw: &[u8], comm: &str) -> String {
    let s: Vec<String> =
        raw.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    if s.is_empty() { format!("[{comm}]") } else { s.join(" ") }
}

pub fn process_row(
    stat_text: &str,
    uid: u32,
    user: String,
    cmdline_raw: &[u8],
    fds: Option<u32>,
    env: &LinuxEnv,
) -> anyhow::Result<Process> {
    let st = parse_stat(stat_text)?;
    let ns_per_tick = 1_000_000_000 / env.ticks.max(1);
    let state = ProcState::from_linux(st.state);
    Ok(Process {
        space: 0,
        pid: st.pid,
        ppid: st.ppid,
        pgid: st.pgrp,
        uid,
        user,
        cmdline: if state == ProcState::Zombie { String::new() } else { cmdline(cmdline_raw, &st.comm) },
        name: st.comm,
        state,
        nice: st.nice as i32,
        priority: st.priority as i32,
        cpu_time_ns: (st.utime + st.stime) * ns_per_tick,
        cpu_percent: 0.0,
        rss: st.rss * env.page_size,
        vsz: st.vsize,
        threads: st.num_threads.max(0) as u32,
        fds,
        start_time: env.btime + st.starttime / env.ticks.max(1),
        // stat is world-readable, so every column is real even for other
        // users' processes; only fd/, environ, maps... are ptrace-gated.
        restricted: false,
        io: None,
        io_rate: None,
        origin: None,
    })
}

/// Per-core counters and boot time from /proc/stat.
pub fn proc_stat(text: &str) -> (Vec<CpuTicks>, u64) {
    let mut cores = Vec::new();
    let mut btime = 0;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some(k) if k.starts_with("cpu") && k.len() > 3 => {
                let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
                // user nice system idle iowait irq softirq steal (guest is inside user)
                let get = |i: usize| v.get(i).copied().unwrap_or(0);
                let idle = get(3) + get(4);
                let busy = get(0) + get(1) + get(2) + get(5) + get(6) + get(7);
                cores.push(CpuTicks { busy, total: busy + idle });
            }
            Some("btime") => btime = it.next().and_then(|x| x.parse().ok()).unwrap_or(0),
            _ => {}
        }
    }
    (cores, btime)
}

pub fn system(stat: &str, meminfo: &str, loadavg: &str, uptime: &str) -> SystemInfo {
    let (cores, _) = proc_stat(stat);
    let mut s = SystemInfo { cores, ..Default::default() };
    if let Ok(m) = procfs_core::Meminfo::from_read(meminfo.as_bytes()) {
        s.mem_total = m.mem_total;
        // "used" the way free(1) means it: everything not reclaimable on demand.
        let available = m.mem_available.unwrap_or(m.mem_free + m.cached + m.buffers);
        s.mem_used = m.mem_total.saturating_sub(available);
        s.swap_total = m.swap_total;
        s.swap_used = m.swap_total.saturating_sub(m.swap_free);
    }
    if let Ok(l) = procfs_core::LoadAverage::from_read(loadavg.as_bytes()) {
        s.load = [l.one as f64, l.five as f64, l.fifteen as f64];
        s.run_queue = Some((l.cur, l.max));
    }
    if let Ok(u) = procfs_core::Uptime::from_read(uptime.as_bytes()) {
        s.uptime_secs = u.uptime as u64;
    }
    s
}

/// Everything read from /proc for one process. Missing files (permission,
/// old kernel, process gone) are `None`/empty.
#[derive(Default, Debug, Clone)]
pub struct ProcFiles {
    pub pid: Pid,
    pub stat: String,
    pub status: String,
    pub cmdline: Vec<u8>,
    pub environ: Option<Vec<u8>>,
    pub io: Option<String>,
    pub limits: Option<String>,
    pub cgroup: Option<String>,
    /// (file name, contents) from /sys/fs/cgroup/<path>/.
    pub cgroup_files: Vec<(String, String)>,
    pub schedstat: Option<String>,
    pub smaps_rollup: Option<String>,
    pub maps: Option<String>,
    pub wchan: Option<String>,
    pub oom_score: Option<String>,
    pub oom_score_adj: Option<String>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    /// (name, link target) of /proc/<pid>/ns/*.
    pub ns: Vec<(String, String)>,
    /// The same for rtop itself, to show which namespaces differ.
    pub self_ns: Vec<(String, String)>,
    /// (fd, link target) of /proc/<pid>/fd/*; None when not permitted.
    pub fds: Option<Vec<(i32, String)>>,
    /// (tid, stat line) of /proc/<pid>/task/*/stat.
    pub tasks: Vec<(u64, String)>,
    /// socket inode -> (protocol, description), from /proc/<pid>/net/*.
    pub sockets: HashMap<u64, (String, String)>,
}

pub fn details(f: &ProcFiles, env: &LinuxEnv, user: &mut dyn FnMut(u32) -> String) -> anyhow::Result<ProcessDetails> {
    let st = parse_stat(&f.stat)?;
    let status = Status::from_read(f.status.as_bytes()).ok();
    let pid = f.pid;
    let p = |file: &str| format!("/proc/{pid}/{file}");
    let ns_per_tick = 1_000_000_000 / env.ticks.max(1);
    let mut d = ProcessDetails { pid, name: st.comm.clone(), ..Default::default() };

    // ---- Overview
    let o = &mut d.overview;
    o.push(Field::new("pid", "PID", st.pid, format!("{} field 1", p("stat"))).hint(stat_hint("pid")));
    o.push(Field::new("ppid", "Parent PID", st.ppid, format!("{} field 4", p("stat"))).hint(stat_hint("ppid")));
    o.push(Field::new("pgid", "Process group", st.pgrp, format!("{} field 5", p("stat"))).hint(stat_hint("pgrp")));
    o.push(Field::new("sid", "Session", st.session, format!("{} field 6", p("stat"))).hint(stat_hint("session")));
    if let Some(s) = &status {
        o.push(
            Field::new(
                "uid",
                "User (effective)",
                format!("{} ({})", user(s.euid), s.euid),
                format!("{} Uid: (real, effective, saved, fs)", p("status")),
            )
            .hint("Uid:"),
        );
        o.push(
            Field::new("uid", "Real / saved UID", format!("{} / {}", s.ruid, s.suid), format!("{} Uid:", p("status")))
                .hint("Uid:"),
        );
        if let Some(nspid) = s.nspid.as_ref().filter(|v| v.len() > 1) {
            let chain: Vec<String> = nspid.iter().map(|x| x.to_string()).collect();
            o.push(
                Field::new("nspid", "PID in each PID namespace", chain.join(" → "), format!("{} NSpid:", p("status")))
                    .hint("NSpid:"),
            );
        }
        if s.tracerpid != 0 {
            o.push(
                Field::new("tracer", "Traced by PID", s.tracerpid, format!("{} TracerPid:", p("status")))
                    .hint("TracerPid:"),
            );
        }
    }
    o.push(
        Field::new("comm", "Name (comm)", &st.comm, format!("{} field 2 / {}", p("stat"), p("comm")))
            .hint(stat_hint("comm")),
    );
    o.push(Field::new(
        "exe",
        "Executable",
        f.exe.as_deref().unwrap_or("— (not permitted)"),
        format!("readlink {}", p("exe")),
    ));
    o.push(Field::new(
        "cwd",
        "Working directory",
        f.cwd.as_deref().unwrap_or("— (not permitted)"),
        format!("readlink {}", p("cwd")),
    ));
    o.push(Field::new("cmdline", "Command line", cmdline(&f.cmdline, &st.comm), p("cmdline")));
    let state = ProcState::from_linux(st.state);
    o.push(
        Field::new(
            "state",
            "State",
            format!("{} — {}", state.letter(), state.label()),
            format!("{} field 3", p("stat")),
        )
        .hint(stat_hint("state")),
    );
    if (state == ProcState::DiskSleep || state == ProcState::Sleeping)
        && let Some(w) = f.wchan.as_deref().filter(|w| !w.is_empty() && *w != "0")
    {
        o.push(Field::new("wchan", "Waiting in kernel function", w, p("wchan")).hint("wchan"));
    }
    o.push(Field::new("nice", "Nice", st.nice, format!("{} field 19", p("stat"))).hint(stat_hint("nice")));
    o.push(
        Field::new("priority", "Priority", st.priority, format!("{} field 18", p("stat"))).hint(stat_hint("priority")),
    );
    o.push(
        Field::new("threads", "Threads", st.num_threads, format!("{} field 20", p("stat")))
            .hint(stat_hint("num_threads")),
    );
    let start = env.btime + st.starttime / env.ticks.max(1);
    o.push(
        Field::new(
            "start_time",
            "Started",
            format!("{start} ({})", fmt::age(start)),
            format!("{} field 22 (ticks after boot) / {} + btime", p("stat"), env.ticks),
        )
        .hint(stat_hint("starttime")),
    );
    o.push(
        Field::new(
            "cpu_time",
            "CPU time user / system",
            format!("{} / {}", fmt::cpu_time(st.utime * ns_per_tick), fmt::cpu_time(st.stime * ns_per_tick)),
            format!("{} fields 14, 15 (clock ticks, {} per second)", p("stat"), env.ticks),
        )
        .hint(stat_hint("utime")),
    );
    if let Some(s) = &status
        && let (Some(v), Some(n)) = (s.voluntary_ctxt_switches, s.nonvoluntary_ctxt_switches)
    {
        o.push(
            Field::new(
                "ctx_switches",
                "Context switches voluntary / involuntary",
                format!("{v} / {n}"),
                format!("{} voluntary_ctxt_switches / nonvoluntary_ctxt_switches", p("status")),
            )
            .hint("voluntary_ctxt_switches"),
        );
    }
    o.push(
        Field::new(
            "faults",
            "Page faults minor / major",
            format!("{} / {}", st.minflt, st.majflt),
            format!("{} fields 10, 12", p("stat")),
        )
        .hint(stat_hint("minflt")),
    );
    o.push(
        Field::new(
            "tty",
            "Controlling TTY",
            if st.tty_nr == 0 {
                "none".to_string()
            } else {
                format!("dev {}:{}", (st.tty_nr >> 8) & 0xfff, st.tty_nr & 0xff)
            },
            format!("{} field 7", p("stat")),
        )
        .hint(stat_hint("tty_nr")),
    );

    // ---- Memory
    let m = &mut d.memory;
    let kb = |v: Option<u64>| v.map(|x| fmt::bytes_exact(x * 1024));
    if let Some(s) = &status {
        for (key, label, val, name) in [
            ("vsz", "Virtual size (VSZ)", s.vmsize, "VmSize"),
            ("rss", "Resident (RSS)", s.vmrss, "VmRSS"),
            ("rss_anon", "  RSS anonymous", s.rssanon, "RssAnon"),
            ("rss_file", "  RSS file-backed", s.rssfile, "RssFile"),
            ("rss_shmem", "  RSS shared memory", s.rssshmem, "RssShmem"),
            ("hwm", "Peak RSS (high water mark)", s.vmhwm, "VmHWM"),
            ("swap", "Swapped out", s.vmswap, "VmSwap"),
            ("vm_data", "Data + heap", s.vmdata, "VmData"),
            ("vm_stack", "Stack", s.vmstk, "VmStk"),
            ("vm_lib", "Shared libraries", s.vmlib, "VmLib"),
            ("vm_pte", "Page tables", s.vmpte, "VmPTE"),
            ("vm_lck", "Locked (mlock)", s.vmlck, "VmLck"),
        ] {
            if let Some(v) = kb(val) {
                m.push(Field::new(key, label, v, format!("{} {name}:", p("status"))).hint(format!("{name}:")));
            }
        }
    } else {
        m.push(Field::new("vsz", "Virtual size (VSZ)", fmt::bytes_exact(st.vsize), format!("{} field 23", p("stat"))));
        m.push(Field::new(
            "rss",
            "Resident (RSS)",
            fmt::bytes_exact(st.rss * env.page_size),
            format!("{} field 24 × page size", p("stat")),
        ));
    }
    if let Some(roll) = f.smaps_rollup.as_deref().and_then(|t| SmapsRollup::from_read(t.as_bytes()).ok())
        && let Some(mm) = roll.memory_map_rollup.0.first()
    {
        let g = |k: &str| mm.extension.map.get(k).copied();
        if let Some(pss) = g("Pss") {
            m.push(
                Field::new("pss", "Proportional (PSS)", fmt::bytes_exact(pss), format!("{} Pss:", p("smaps_rollup")))
                    .hint("Pss:"),
            );
        }
        if let (Some(c), Some(dirty)) = (g("Private_Clean"), g("Private_Dirty")) {
            m.push(
                Field::new(
                    "uss",
                    "Unique (USS)",
                    fmt::bytes_exact(c + dirty),
                    format!("{} Private_Clean + Private_Dirty", p("smaps_rollup")),
                )
                .hint("Private_Dirty:"),
            );
        }
    }
    if let Some(io) = &f.io {
        let kv = key_values(io, ':');
        for (key, label, name) in [
            ("io_rchar", "Bytes read (any read syscall)", "rchar"),
            ("io_rchar", "Bytes written (any write syscall)", "wchar"),
            ("io_read", "Bytes read from storage", "read_bytes"),
            ("io_write", "Bytes written to storage", "write_bytes"),
        ] {
            if let Some(v) = kv.get(name).and_then(|v| v.parse::<u64>().ok()) {
                m.push(
                    Field::new(key, label, fmt::bytes_exact(v), format!("{} {name}", p("io"))).hint(format!("{name}:")),
                );
            }
        }
    }
    if let Some(s) = &f.oom_score {
        let adj = f.oom_score_adj.as_deref().map_or("?", str::trim);
        m.push(Field::new(
            "oom_score",
            "OOM score (adj)",
            format!("{} ({adj})", s.trim()),
            format!("{} / oom_score_adj", p("oom_score")),
        ));
    }

    // ---- Regions, files, threads, env
    if let Some(maps) = &f.maps {
        d.regions = parse_maps(maps);
    }
    match &f.fds {
        Some(fds) => {
            d.files = fds
                .iter()
                .map(|(fd, target)| {
                    let (kind, target) = describe_fd(target, &f.sockets);
                    OpenFile { fd: *fd, kind, target }
                })
                .collect()
        }
        None => d.notes.push(format!("{} is not readable: only the owner (or CAP_SYS_PTRACE) may list fds.", p("fd"))),
    }
    d.threads = f
        .tasks
        .iter()
        .filter_map(|(tid, text)| {
            let t = parse_stat(text).ok()?;
            Some(ThreadInfo {
                tid: *tid,
                name: t.comm,
                state: ProcState::from_linux(t.state).label().into(),
                cpu_time_ns: Some((t.utime + t.stime) * ns_per_tick),
                priority: Some(t.priority as i32),
            })
        })
        .collect();
    match &f.environ {
        Some(e) => {
            d.env = e
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let s = String::from_utf8_lossy(s);
                    match s.split_once('=') {
                        Some((k, v)) => (k.to_string(), v.to_string()),
                        None => (s.into_owned(), String::new()),
                    }
                })
                .collect()
        }
        None => d.notes.push(format!("{} is not readable: environment is private to the owner.", p("environ"))),
    }
    if let Some(l) = &f.limits {
        d.limits = parse_limits(l, &p("limits"));
    }

    d.kernel = kernel(f, &st, status.as_ref(), &p);
    d.raw = raw(f, &p);
    Ok(d)
}

fn kernel(f: &ProcFiles, st: &Stat, status: Option<&Status>, p: &dyn Fn(&str) -> String) -> KernelView {
    let mut sections = Vec::new();

    let mine: HashMap<&str, &str> = f.self_ns.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    sections.push((
        "Namespaces".to_string(),
        f.ns.iter()
            .map(|(name, target)| {
                let cmp = match mine.get(name.as_str()) {
                    Some(m) if *m == target => "same as rtop",
                    Some(_) => "DIFFERENT from rtop",
                    None => "",
                };
                Field::new(
                    "namespaces",
                    name.clone(),
                    format!("{target}  {cmp}"),
                    format!("readlink {}", p(&format!("ns/{name}"))),
                )
                .hint(format!("{name} -> "))
            })
            .collect(),
    ));

    let mut cg = Vec::new();
    if let Some(text) = &f.cgroup {
        for line in text.lines() {
            let mut parts = line.splitn(3, ':');
            let (h, ctrl, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let label = if h == "0" && ctrl.is_empty() { "cgroup v2 path".to_string() } else { format!("v1 {ctrl}") };
            cg.push(Field::new("cgroup", label, path, p("cgroup")).hint(line.to_string()));
        }
    }
    for (name, val) in &f.cgroup_files {
        let key = if name.ends_with(".pressure") { "psi" } else { "cgroup" };
        let v = val.lines().next().unwrap_or("").to_string();
        cg.push(Field::new(key, name.clone(), v, format!("/sys/fs/cgroup/<path>/{name}")));
    }
    sections.push(("Control group".to_string(), cg));

    if let Some(s) = status {
        let mut sec = vec![
            Field::new("capabilities", "CapEff (in use)", cap_names(s.capeff), format!("{} CapEff:", p("status")))
                .hint("CapEff:"),
            Field::new("capabilities", "CapPrm (permitted)", cap_names(s.capprm), format!("{} CapPrm:", p("status")))
                .hint("CapPrm:"),
            Field::new("capabilities", "CapInh (inheritable)", cap_names(s.capinh), format!("{} CapInh:", p("status")))
                .hint("CapInh:"),
        ];
        if let Some(b) = s.capbnd {
            sec.push(
                Field::new("capabilities", "CapBnd (bounding set)", cap_names(b), format!("{} CapBnd:", p("status")))
                    .hint("CapBnd:"),
            );
        }
        if let Some(a) = s.capamb {
            sec.push(
                Field::new("capabilities", "CapAmb (ambient)", cap_names(a), format!("{} CapAmb:", p("status")))
                    .hint("CapAmb:"),
            );
        }
        if let Some(n) = s.nonewprivs {
            sec.push(
                Field::new(
                    "no_new_privs",
                    "NoNewPrivs",
                    if n == 1 { "1 — set-uid/file caps ignored on exec" } else { "0" },
                    format!("{} NoNewPrivs:", p("status")),
                )
                .hint("NoNewPrivs:"),
            );
        }
        if let Some(sc) = s.seccomp {
            let mode = match sc {
                0 => "0 — disabled",
                1 => "1 — strict (read/write/exit only)",
                2 => "2 — filter (BPF program decides per syscall)",
                _ => "?",
            };
            sec.push(Field::new("seccomp", "Seccomp", mode, format!("{} Seccomp:", p("status"))).hint("Seccomp:"));
        }
        sections.push(("Privileges & sandboxing".to_string(), sec));
    }

    let mut sched = vec![
        Field::new("policy", "Policy", policy_name(st.policy.unwrap_or(0)), format!("{} field 41", p("stat")))
            .hint(stat_hint("policy")),
        Field::new("rt_priority", "Real-time priority", st.rt_priority.unwrap_or(0), format!("{} field 40", p("stat")))
            .hint(stat_hint("rt_priority")),
        Field::new("processor", "Last ran on CPU", st.processor.unwrap_or(-1), format!("{} field 39", p("stat")))
            .hint(stat_hint("processor")),
    ];
    if let Some(list) = status.and_then(|s| s.cpus_allowed_list.as_ref()) {
        let r: Vec<String> =
            list.iter().map(|(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") }).collect();
        sched.push(
            Field::new(
                "cpus_allowed",
                "Allowed CPUs (affinity)",
                r.join(","),
                format!("{} Cpus_allowed_list:", p("status")),
            )
            .hint("Cpus_allowed_list:"),
        );
    }
    if let Some(ss) = &f.schedstat {
        let v: Vec<u64> = ss.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() >= 3 {
            sched.push(
                Field::new("schedstat", "Time on CPU", fmt::cpu_time(v[0]), format!("{} field 1 (ns)", p("schedstat")))
                    .hint("on_cpu_ns"),
            );
            sched.push(
                Field::new(
                    "schedstat",
                    "Time waiting on run queue",
                    fmt::cpu_time(v[1]),
                    format!("{} field 2 (ns)", p("schedstat")),
                )
                .hint("runq_wait_ns"),
            );
            sched.push(
                Field::new("schedstat", "Timeslices", v[2], format!("{} field 3", p("schedstat"))).hint("timeslices"),
            );
        }
    }
    if let Some(w) = f.wchan.as_deref().filter(|w| !w.is_empty() && *w != "0") {
        sched.push(Field::new("wchan", "Wait channel", w, p("wchan")).hint("wchan"));
    }
    sections.push(("Scheduling".to_string(), sched));

    if let Some(s) = status {
        sections.push((
            "Signals".to_string(),
            vec![
                Field::new("signals", "Blocked", signal_names(s.sigblk), format!("{} SigBlk:", p("status")))
                    .hint("SigBlk:"),
                Field::new("signals", "Ignored", signal_names(s.sigign), format!("{} SigIgn:", p("status")))
                    .hint("SigIgn:"),
                Field::new(
                    "signals",
                    "Caught (has handler)",
                    signal_names(s.sigcgt),
                    format!("{} SigCgt:", p("status")),
                )
                .hint("SigCgt:"),
                Field::new(
                    "signals",
                    "Pending",
                    signal_names(s.shdpnd | s.sigpnd),
                    format!("{} SigPnd/ShdPnd:", p("status")),
                )
                .hint("ShdPnd:"),
            ],
        ));
    }
    KernelView { sections, note: None }
}

fn raw(f: &ProcFiles, p: &dyn Fn(&str) -> String) -> Vec<RawSource> {
    let mut v = vec![RawSource { title: p("stat"), body: annotate_stat(&f.stat) }];
    v.push(RawSource { title: p("status"), body: f.status.clone() });
    let opt = |name: &str, body: &Option<String>| body.as_ref().map(|b| RawSource { title: p(name), body: b.clone() });
    v.extend(opt("io", &f.io));
    v.extend(opt("limits", &f.limits));
    v.extend(opt("cgroup", &f.cgroup));
    if let Some(s) = &f.schedstat {
        let names = ["on_cpu_ns", "runq_wait_ns", "timeslices"];
        let ann: String = s.split_whitespace().zip(names).map(|(x, n)| format!("  {n:<14} = {x}\n")).collect();
        v.push(RawSource { title: p("schedstat"), body: format!("{}\n{ann}", s.trim()) });
    }
    v.extend(opt("smaps_rollup", &f.smaps_rollup));
    if let Some(w) = &f.wchan {
        v.push(RawSource { title: p("wchan"), body: format!("wchan = {w}\n") });
    }
    if !f.ns.is_empty() {
        let body = f.ns.iter().map(|(n, t)| format!("{n} -> {t}\n")).collect();
        v.push(RawSource { title: format!("ls -l {}", p("ns/")), body });
    }
    if let Some(m) = &f.maps {
        let lines: Vec<&str> = m.lines().collect();
        let mut body = lines.iter().take(80).map(|l| format!("{l}\n")).collect::<String>();
        if lines.len() > 80 {
            body.push_str(&format!("… {} more lines (full list in Memory tab)\n", lines.len() - 80));
        }
        v.push(RawSource { title: p("maps"), body });
    }
    v
}

/// The raw stat line followed by one numbered line per field.
pub fn annotate_stat(stat: &str) -> String {
    let mut out = format!("{}\n\n", stat.trim_end());
    let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
        return out;
    };
    let mut fields = vec![stat[..open].trim(), &stat[open..=close]];
    fields.extend(stat[close + 1..].split_whitespace());
    for (i, (v, name)) in fields.iter().zip(STAT_FIELDS).enumerate() {
        out.push_str(&format!("{:>3}  {name:<22} = {v}\n", i + 1));
    }
    out
}

pub fn parse_maps(text: &str) -> Vec<Region> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let range = it.next()?;
            let perms = it.next()?.to_string();
            let _offset = it.next()?;
            let _dev = it.next()?;
            let inode = it.next()?;
            let path = it.collect::<Vec<_>>().join(" ");
            let (a, b) = range.split_once('-')?;
            let kind = match path.as_str() {
                "" => "anon",
                "[heap]" => "heap",
                "[stack]" => "stack",
                "[vdso]" | "[vvar]" | "[vsyscall]" => "kernel",
                _ if inode != "0" => "file",
                _ => "special",
            };
            Some(Region {
                start: u64::from_str_radix(a, 16).ok()?,
                end: u64::from_str_radix(b, 16).ok()?,
                perms,
                kind: kind.into(),
                resident: None,
                path,
            })
        })
        .collect()
}

/// /proc/<pid>/limits is a fixed-width table: name, soft, hard, units.
pub fn parse_limits(text: &str, source: &str) -> Vec<Field> {
    text.lines()
        .skip(1)
        .filter(|l| l.len() > 26)
        .map(|line| {
            let name = line[..26].trim().to_string();
            let rest: Vec<&str> = line[26..].split_whitespace().collect();
            let (soft, hard) = (rest.first().copied().unwrap_or("?"), rest.get(1).copied().unwrap_or("?"));
            let units = rest.get(2).copied().unwrap_or("");
            let key = if name == "Max open files" { "limit_nofile" } else { "limits" };
            Field::new(key, name.clone(), format!("soft {soft} / hard {hard} {units}"), format!("{source} \"{name}\""))
                .hint(name)
        })
        .collect()
}

/// `read_bytes` and `write_bytes` from `/proc/PID/io`: what the process
/// made the block layer move, as opposed to `rchar`/`wchar` (every byte
/// through read()/write(), page cache hits and pipes included).
pub fn io_bytes(text: &str) -> Option<[u64; 2]> {
    let kv = key_values(text, ':');
    let get = |k: &str| kv.get(k)?.parse().ok();
    Some([get("read_bytes")?, get("write_bytes")?])
}

fn key_values(text: &str, sep: char) -> HashMap<String, String> {
    text.lines().filter_map(|l| l.split_once(sep)).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

fn describe_fd(target: &str, sockets: &HashMap<u64, (String, String)>) -> (String, String) {
    if let Some(ino) = target.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']')) {
        if let Some((proto, desc)) = ino.parse().ok().and_then(|i: u64| sockets.get(&i)) {
            return (proto.clone(), desc.clone());
        }
        return ("SOCK".into(), format!("inode {ino}"));
    }
    let kind = if target.starts_with("pipe:") {
        "FIFO"
    } else if target.starts_with("anon_inode:") {
        "a_inode"
    } else if target.starts_with("/dev/") {
        "CHR"
    } else if target.starts_with('/') {
        "REG"
    } else {
        "?"
    };
    (kind.into(), target.into())
}

/// Parses /proc/net/{tcp,tcp6,udp,udp6} into inode -> (protocol, "local → remote STATE").
pub fn parse_net(text: &str, proto: &str, v6: bool) -> Vec<(u64, (String, String))> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let c: Vec<&str> = line.split_whitespace().collect();
            let local = parse_sockaddr(c.get(1)?, v6)?;
            let remote = parse_sockaddr(c.get(2)?, v6)?;
            let state = u8::from_str_radix(c.get(3)?, 16).ok()?;
            let inode: u64 = c.get(9)?.parse().ok()?;
            let st = if proto.starts_with("TCP") { tcp_state(state) } else { "" };
            Some((inode, (proto.to_string(), format!("{local} → {remote}  {st}").trim_end().to_string())))
        })
        .collect()
}

/// Kernel prints addresses as raw hex of the in-memory bytes, so each 32-bit
/// word is in host (little-endian) order.
fn parse_sockaddr(s: &str, v6: bool) -> Option<String> {
    let (addr, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = Vec::new();
    for chunk in addr.as_bytes().chunks(8) {
        let word = u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    let ip = if v6 {
        let b: [u8; 16] = bytes.try_into().ok()?;
        format!("[{}]", Ipv6Addr::from(b))
    } else {
        let b: [u8; 4] = bytes.try_into().ok()?;
        Ipv4Addr::from(b).to_string()
    };
    Some(if port == 0 { format!("{ip}:*") } else { format!("{ip}:{port}") })
}

pub fn parse_unix(text: &str) -> Vec<(u64, (String, String))> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let c: Vec<&str> = line.split_whitespace().collect();
            let inode: u64 = c.get(6)?.parse().ok()?;
            let path = c.get(7).map_or("(unnamed)".to_string(), |p| p.to_string());
            Some((inode, ("UNIX".to_string(), path)))
        })
        .collect()
}

fn tcp_state(s: u8) -> &'static str {
    match s {
        0x01 => "ESTABLISHED",
        0x02 => "SYN_SENT",
        0x03 => "SYN_RECV",
        0x04 => "FIN_WAIT1",
        0x05 => "FIN_WAIT2",
        0x06 => "TIME_WAIT",
        0x07 => "CLOSE",
        0x08 => "CLOSE_WAIT",
        0x09 => "LAST_ACK",
        0x0A => "LISTEN",
        0x0B => "CLOSING",
        _ => "?",
    }
}

const CAPS: [&str; 41] = [
    "chown",
    "dac_override",
    "dac_read_search",
    "fowner",
    "fsetid",
    "kill",
    "setgid",
    "setuid",
    "setpcap",
    "linux_immutable",
    "net_bind_service",
    "net_broadcast",
    "net_admin",
    "net_raw",
    "ipc_lock",
    "ipc_owner",
    "sys_module",
    "sys_rawio",
    "sys_chroot",
    "sys_ptrace",
    "sys_pacct",
    "sys_admin",
    "sys_boot",
    "sys_nice",
    "sys_resource",
    "sys_time",
    "sys_tty_config",
    "mknod",
    "lease",
    "audit_write",
    "audit_control",
    "setfcap",
    "mac_override",
    "mac_admin",
    "syslog",
    "wake_alarm",
    "block_suspend",
    "audit_read",
    "perfmon",
    "bpf",
    "checkpoint_restore",
];

pub fn cap_names(mask: u64) -> String {
    if mask == 0 {
        return "none (0x0)".into();
    }
    let all = (1u64 << CAPS.len()) - 1;
    if mask & all == all {
        return format!("ALL {} capabilities (0x{mask:x})", CAPS.len());
    }
    let names: Vec<&str> = CAPS.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, n)| *n).collect();
    format!("{} (0x{mask:x})", names.join(","))
}

const SIGNALS: [&str; 31] = [
    "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "BUS", "FPE", "KILL", "USR1", "SEGV", "USR2", "PIPE", "ALRM", "TERM",
    "STKFLT", "CHLD", "CONT", "STOP", "TSTP", "TTIN", "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO",
    "PWR", "SYS",
];

pub fn signal_names(mask: u64) -> String {
    if mask == 0 {
        return "none".into();
    }
    let mut names: Vec<String> =
        SIGNALS.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, n)| n.to_string()).collect();
    let rt = (31..64).filter(|i| mask & (1 << i) != 0).count();
    if rt > 0 {
        names.push(format!("+{rt} real-time"));
    }
    names.join(" ")
}

fn policy_name(p: u32) -> String {
    match p {
        0 => "SCHED_OTHER (0) — CFS/EEVDF fair scheduler".into(),
        1 => "SCHED_FIFO (1) — real-time, runs until it yields".into(),
        2 => "SCHED_RR (2) — real-time, round-robin slices".into(),
        3 => "SCHED_BATCH (3)".into(),
        5 => "SCHED_IDLE (5)".into(),
        6 => "SCHED_DEADLINE (6)".into(),
        p => p.to_string(),
    }
}

/// The CPU's marketing name and its average clock from /proc/cpuinfo. ARM
/// kernels list no "model name", only implementer/part numbers.
pub fn cpuinfo(text: &str) -> (Option<String>, Option<f32>) {
    let mut model = None;
    let mut mhz = Vec::new();
    for (k, v) in text.lines().filter_map(|l| l.split_once(':')) {
        match k.trim() {
            "model name" if model.is_none() => model = Some(v.trim().to_string()),
            "cpu MHz" => mhz.extend(v.trim().parse::<f32>().ok()),
            _ => {}
        }
    }
    let avg = (!mhz.is_empty()).then(|| mhz.iter().sum::<f32>() / mhz.len() as f32);
    (model, avg)
}

/// A kernel CPU list such as "0-3,8,10-11".
pub fn cpulist(text: &str) -> Vec<usize> {
    text.trim()
        .split(',')
        .filter_map(|part| {
            let (a, b) = part.split_once('-').unwrap_or((part, part));
            Some(a.parse().ok()?..=b.parse().ok()?)
        })
        .flatten()
        .collect()
}

/// A battery from the attribute files of /sys/class/power_supply/BAT*/
/// (name -> contents). Energy is in µWh, charge in µAh, voltage in µV,
/// current in µA, power in µW, temp in tenths of a degree.
pub fn battery(attrs: &HashMap<String, String>, source: &str) -> Option<Battery> {
    let num = |k: &str| attrs.get(k)?.trim().parse::<i64>().ok();
    let percent = num("capacity")? as f32;
    let status = attrs.get("status").map_or("", |s| s.trim());
    let capacity = [("energy_full", "energy_full_design", "mWh"), ("charge_full", "charge_full_design", "mAh")]
        .into_iter()
        .find_map(|(f, d, unit)| Some((num(f)? as u64 / 1000, num(d)? as u64 / 1000, unit)))
        .filter(|&(_, d, _)| d > 0);
    let current = num("current_now").map(|ua| (ua / 1000) as i32);
    let discharging = status == "Discharging";
    // Many drivers report current_now unsigned; sign it from the status.
    let current = current.map(|ma| if discharging { -ma.abs() } else { ma.abs() });
    let watts = num("power_now")
        .map(|uw| uw as f32 / 1e6)
        .or_else(|| Some(num("voltage_now")? as f32 / 1e6 * current?.unsigned_abs() as f32 / 1e3));
    let minutes = ["time_to_full_now", "time_to_empty_now"]
        .into_iter()
        .find_map(&num)
        .filter(|&s| s > 0)
        .map(|s| (s / 60) as u32);
    Some(Battery {
        percent,
        state: status.to_lowercase(),
        minutes,
        capacity,
        cycles: num("cycle_count").filter(|&c| c > 0).map(|c| c as u32),
        voltage_mv: num("voltage_now").map(|uv| (uv / 1000) as u32),
        current_ma: current,
        temp_c: num("temp").map(|t| t as f32 / 10.0),
        system_watts: watts.filter(|&w| discharging && w > 0.0),
        adapter_watts: None,
        source: source.to_string(),
    })
}

/// (interface, bytes received, bytes sent) from /proc/net/dev. After two
/// header lines each row is `name: rx_bytes rx_packets ... (8 rx fields)
/// tx_bytes ...`.
pub fn parse_net_dev(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let n: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            Some((name.trim().to_string(), *n.first()?, *n.get(8)?))
        })
        .collect()
}

/// Pages swapped in and out since boot (`pswpin`, `pswpout` in
/// /proc/vmstat), as bytes.
pub fn vmstat_swap(text: &str, page_size: u64, source: &str) -> Option<IoCounters> {
    let kv = key_values(text, ' ');
    let get = |k: &str| kv.get(k)?.trim().parse::<u64>().ok();
    Some(IoCounters { read: get("pswpin")? * page_size, write: get("pswpout")? * page_size, source: source.into() })
}

/// /proc/pressure/memory: `some avg10=1.21 avg60=… total=…` and a `full`
/// line. Missing when the kernel was built without PSI or booted with
/// `psi=0`.
pub fn psi(text: &str) -> Option<Pressure> {
    let avg10 = |kind: &str| {
        let line = text.lines().find(|l| l.starts_with(kind))?;
        line.split_whitespace().find_map(|f| f.strip_prefix("avg10="))?.parse::<f32>().ok()
    };
    Some(Pressure::Psi { some10: avg10("some ")?, full10: avg10("full ").unwrap_or(0.0) })
}

/// The interface of the default route: the first /proc/net/route row
/// whose destination is 00000000 (0.0.0.0/0), lowest metric first.
pub fn default_route_iface(text: &str) -> Option<String> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if *f.get(1)? != "00000000" {
                return None;
            }
            Some((f.get(6)?.parse::<u32>().ok()?, f[0].to_string()))
        })
        .min()
        .map(|(_, name)| name)
}

/// One line of /proc/self/mountinfo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// `major:minor` of the device: the same filesystem bind-mounted twice
    /// has the same one.
    pub dev: String,
    pub mount: String,
    pub fstype: String,
    pub source: String,
}

/// /proc/self/mountinfo: `id parent major:minor root mountpoint options
/// [optional fields...] - fstype source superoptions`. Spaces in paths are
/// written as `\040`.
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    let unescape = |s: &str| {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('\\') {
            out.push_str(&rest[..i]);
            let code = rest.get(i + 1..i + 4).and_then(|o| u8::from_str_radix(o, 8).ok());
            match code {
                Some(c) => {
                    out.push(c as char);
                    rest = &rest[i + 4..];
                }
                None => {
                    out.push('\\');
                    rest = &rest[i + 1..];
                }
            }
        }
        out + rest
    };
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            let r: Vec<&str> = right.split(' ').collect();
            Some(Mount {
                dev: f.get(2)?.to_string(),
                mount: unescape(f.get(4)?),
                fstype: r.first()?.to_string(),
                source: unescape(r.get(1)?),
            })
        })
        .collect()
}

/// Kernel interfaces and memory-backed filesystems: no disk space to show.
pub const PSEUDO_FS: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "ramfs",
    "cgroup",
    "cgroup2",
    "mqueue",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "bpf",
    "configfs",
    "fusectl",
    "hugetlbfs",
    "autofs",
    "binfmt_misc",
    "nsfs",
    "rpc_pipefs",
    "efivarfs",
    "selinuxfs",
    "squashfs",
    "nfsd",
    "devfs",
    "shm",
];

/// statvfs(3) on these waits for a server: a hung NFS mount would freeze
/// the UI, so rtop lists them without reading them.
fn remote_fs(t: &str) -> bool {
    matches!(t, "nfs" | "nfs4" | "cifs" | "smb3" | "smbfs" | "ceph" | "glusterfs" | "9p" | "afs" | "sshfs")
        || t.starts_with("fuse.")
        || t == "fuse"
}

/// Which mounts to read with statvfs, and the volumes listed without
/// reading. Pseudo filesystems are dropped; a device mounted at several
/// places (bind mounts, Docker's /etc/hosts) is read once, at its shortest
/// mount point.
pub fn plan_volumes(mounts: Vec<Mount>) -> (Vec<Mount>, Vec<Volume>) {
    let mut read: Vec<Mount> = Vec::new();
    let mut skipped = Vec::new();
    for m in mounts {
        if PSEUDO_FS.contains(&m.fstype.as_str()) {
            continue;
        }
        if remote_fs(&m.fstype) {
            skipped.push(Volume {
                mount: m.mount,
                device: m.source,
                fstype: m.fstype,
                note: Some("not read: a network or FUSE filesystem can block statvfs".into()),
                ..Default::default()
            });
            continue;
        }
        match read.iter_mut().find(|r| r.dev == m.dev) {
            Some(r) if m.mount.len() < r.mount.len() => *r = m,
            Some(_) => {}
            None => read.push(m),
        }
    }
    (read, skipped)
}

/// (device, bytes read, bytes written) from /proc/diskstats. Columns 6 and 10
/// count 512-byte sectors whatever the disk's real sector size.
pub fn parse_diskstats(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let sectors = |i: usize| f.get(i)?.parse::<u64>().ok();
            Some((f.get(2)?.to_string(), sectors(5)? * 512, sectors(9)? * 512))
        })
        .collect()
}

/// Counters from a cgroup v2 `cpu.stat`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CgroupCpuStat {
    /// CPU time of every process in the cgroup, in microseconds.
    pub usage_usec: u64,
    /// Scheduler periods (100 ms by default) in which the cgroup ran.
    pub nr_periods: u64,
    /// Periods in which it used up its quota and had to wait.
    pub nr_throttled: u64,
    pub throttled_usec: u64,
}

pub fn cgroup_cpu_stat(text: &str) -> CgroupCpuStat {
    let mut s = CgroupCpuStat::default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(k), Some(v)) = (it.next(), it.next().and_then(|v| v.parse().ok())) else { continue };
        match k {
            "usage_usec" => s.usage_usec = v,
            "nr_periods" => s.nr_periods = v,
            "nr_throttled" => s.nr_throttled = v,
            "throttled_usec" => s.throttled_usec = v,
            _ => {}
        }
    }
    s
}

/// `cpu.max`: `<quota> <period>` in microseconds, as cores (`50000 100000`
/// is 0.5); `max` means no limit.
pub fn cgroup_cpu_max(text: &str) -> Option<f64> {
    let mut it = text.split_whitespace();
    let quota: f64 = it.next()?.parse().ok()?;
    let period: f64 = it.next()?.parse().ok()?;
    (period > 0.0).then(|| quota / period)
}

/// A byte count such as `memory.max`; `max` (no limit) is `None`.
pub fn cgroup_bytes(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// `oom_kill` from `memory.events`: processes the kernel killed because
/// the cgroup reached `memory.max`.
pub fn cgroup_oom_kills(text: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix("oom_kill ")?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn system_fixture(name: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/linux/system").join(name))
            .unwrap()
    }

    #[test]
    fn run_queue_is_field_four_of_loadavg() {
        let s = system("", "", "9.76 6.24 4.43 4/2565 36658\n", "");
        assert_eq!(s.run_queue, Some((4, 2565)));
        assert_eq!(s.load[0], 9.76f32 as f64);
    }

    #[test]
    fn default_route() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\n\
            wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\n\
            eth0\t00000000\t010011AC\t0003\t0\t0\t100\t00000000\n\
            eth0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\n";
        assert_eq!(default_route_iface(route).as_deref(), Some("eth0"), "lowest metric wins");
        assert_eq!(default_route_iface("Iface\tDestination\n"), None);
    }

    #[test]
    fn swap_and_pressure() {
        let vm = system_fixture("vmstat");
        let io = vmstat_swap(&vm, 4096, "/proc/vmstat").unwrap();
        assert_eq!((io.read, io.write), (4 * 4096, 9 * 4096));
        assert_eq!(psi(&system_fixture("pressure_memory")), Some(Pressure::Psi { some10: 1.21, full10: 0.5 }));
        assert_eq!(psi(""), None, "no PSI in this kernel");
    }

    #[test]
    fn mountinfo_keeps_disks_once_and_lists_remote_ones_unread() {
        let mounts = parse_mountinfo(&system_fixture("mountinfo"));
        assert!(mounts.iter().any(|m| m.mount == "/mnt/my disk"), "\\040 is a space");
        let (read, skipped) = plan_volumes(mounts);
        let names: Vec<_> = read.iter().map(|m| (m.mount.as_str(), m.fstype.as_str())).collect();
        assert_eq!(names, [("/", "overlay"), ("/etc/hosts", "ext4"), ("/mnt/my disk", "xfs")]);
        let unread: Vec<_> = skipped.iter().map(|v| v.mount.as_str()).collect();
        assert_eq!(unread, ["/src", "/srv/nfs"]);
        assert!(skipped.iter().all(|v| !v.shown && v.note.is_some()));
    }

    #[test]
    fn space_matches_df() {
        use crate::collector::space::space;
        // df -k: 1K-blocks 10255636, Used 3218004, Available 6497160
        // (blocks 2563909, bfree 1759408, bavail 1624290, frsize 4096).
        let (size, used, free) = space(2563909, 1759408, 1624290, 4096);
        assert_eq!((size / 1024, used / 1024, free / 1024), (10255636, 3218004, 6497160));
        let v = Volume { used, free, ..Default::default() };
        assert_eq!(v.percent().round(), 33.0, "df's Capacity: used / (used + avail)");
    }

    fn fixture(pid: &str, name: &str) -> Option<String> {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/linux").join(pid).join(name))
            .ok()
    }

    const ENV: LinuxEnv = LinuxEnv { ticks: 100, page_size: 4096, btime: 1_700_000_000 };

    #[test]
    fn cpu_and_battery() {
        let (model, mhz) = cpuinfo(
            "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i7\ncpu MHz\t\t: 2000.0\n\nprocessor\t: 1\ncpu MHz\t\t: 3000.0\n",
        );
        assert_eq!((model.as_deref(), mhz), (Some("Intel(R) Core(TM) i7"), Some(2500.0)));
        assert_eq!(cpulist("0-2,5,7-8\n"), vec![0, 1, 2, 5, 7, 8]);
        let attrs: HashMap<String, String> = [
            ("capacity", "57"),
            ("status", "Discharging"),
            ("energy_full", "45000000"),
            ("energy_full_design", "50000000"),
            ("power_now", "8500000"),
            ("cycle_count", "120"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), format!("{v}\n")))
        .collect();
        let b = battery(&attrs, "BAT0").unwrap();
        assert_eq!(
            (b.percent, b.state.as_str(), b.capacity, b.cycles),
            (57.0, "discharging", Some((45000, 50000, "mWh")), Some(120))
        );
        assert_eq!(b.system_watts, Some(8.5));
    }

    #[test]
    fn net_dev_and_diskstats() {
        let dev = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:    1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
  eth0: 5555        7    0    0    0     0          0         0     4444       6    0    0    0     0       0          0
";
        assert_eq!(parse_net_dev(dev)[1], ("eth0".to_string(), 5555, 4444));
        let disks =
            " 254       0 vda 1000 0 2048 0 500 0 4096 0 0 0 0 0 0 0 0 0 0\n   7       0 loop0 1 0 8 0 0 0 0 0 0 0 0\n";
        assert_eq!(parse_diskstats(disks)[0], ("vda".to_string(), 2048 * 512, 4096 * 512));
        assert_eq!(parse_diskstats(disks).len(), 2);
    }

    #[test]
    fn stat_with_spaces_in_comm() {
        let stat = "42 (my (weird) app) S 1 42 42 0 -1 4194560 100 0 3 0 250 50 0 0 20 0 3 0 1000 10485760 512 \
                    18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 2 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let p = process_row(stat, 1000, "u".into(), b"/usr/bin/app\0--flag\0", Some(7), &ENV).unwrap();
        assert_eq!(p.name, "my (weird) app");
        assert_eq!(p.cmdline, "/usr/bin/app --flag");
        assert_eq!(p.cpu_time_ns, 3_000_000_000);
        assert_eq!(p.rss, 512 * 4096);
        assert_eq!(p.start_time, 1_700_000_010);
        assert_eq!(p.threads, 3);
        let ann = annotate_stat(stat);
        assert!(ann.contains(" 14  utime                  = 250"));
        assert!(ann.contains("  2  comm                   = (my (weird) app)"));
    }

    #[test]
    fn kernel_thread_has_bracketed_name() {
        assert_eq!(cmdline(b"", "kworker/0:1"), "[kworker/0:1]");
    }

    #[test]
    fn proc_stat_cores_and_btime() {
        let (cores, btime) = proc_stat(
            "cpu  10 0 10 80 0 0 0 0 0 0\ncpu0 5 0 5 40 0 0 0 0 0 0\ncpu1 5 0 5 40 0 0 0 0 0 0\nbtime 1700000000\n",
        );
        assert_eq!(cores.len(), 2);
        assert_eq!(cores[0], CpuTicks { busy: 10, total: 50 });
        assert_eq!(btime, 1_700_000_000);
    }

    #[test]
    fn net_tables() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
                   0: 0100007F:0CEA 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0 100 0 0 10 0\n";
        let v = parse_net(tcp, "TCP", false);
        assert_eq!(v, vec![(12345, ("TCP".to_string(), "127.0.0.1:3306 → 0.0.0.0:*  LISTEN".to_string()))]);
        let tcp6 = "header\n   0: 00000000000000000000000001000000:0016 00000000000000000000000000000000:0000 0A 0:0 0:0 0 0 0 99 1\n";
        assert_eq!(parse_net(tcp6, "TCP6", true)[0].1.1, "[::1]:22 → [::]:*  LISTEN");
    }

    #[test]
    fn caps_and_signals() {
        assert_eq!(cap_names(0), "none (0x0)");
        assert_eq!(cap_names(1 << 10 | 1 << 13), "net_bind_service,net_raw (0x2400)");
        assert!(cap_names(0x1ff_ffff_ffff).starts_with("ALL"));
        assert_eq!(signal_names(1 << 1 | 1 << 16), "INT CHLD");
    }

    #[test]
    fn limits_table() {
        let text = "Limit                     Soft Limit           Hard Limit           Units     \n\
                    Max open files            1024                 1048576              files     \n";
        let f = parse_limits(text, "/proc/1/limits");
        assert_eq!(f[0].key, "limit_nofile");
        assert_eq!(f[0].value, "soft 1024 / hard 1048576 files");
    }

    #[test]
    fn io_bytes_are_storage_not_syscall_bytes() {
        let dd = fixture("dd", "io").unwrap();
        // 2 GB passed to write(), all of it charged as storage writes; reads
        // came from /dev/zero, so almost nothing was read from storage.
        assert_eq!(io_bytes(&dd), Some([8192, 2_097_152_000]));
        assert_eq!(io_bytes(&fixture("self", "io").unwrap()), Some([0, 0]));
        assert_eq!(io_bytes("rchar: 1\n"), None, "kernel without task I/O accounting");
    }

    #[test]
    fn fixture_process_details() {
        let Some(stat) = fixture("self", "stat") else { return };
        let read = |n| fixture("self", n);
        let files = ProcFiles {
            pid: 1,
            stat,
            status: read("status").unwrap(),
            cmdline: read("cmdline").unwrap_or_default().into_bytes(),
            io: read("io"),
            limits: read("limits"),
            cgroup: read("cgroup"),
            schedstat: read("schedstat"),
            smaps_rollup: read("smaps_rollup"),
            maps: read("maps"),
            ..Default::default()
        };
        let d = details(&files, &ENV, &mut |u| u.to_string()).unwrap();
        let keys: Vec<&str> = d.overview.iter().chain(&d.memory).map(|f| f.key).collect();
        for k in ["pid", "ppid", "state", "uid", "rss", "vsz", "ctx_switches"] {
            assert!(keys.contains(&k), "missing {k}");
        }
        assert!(d.memory.iter().any(|f| f.key == "pss"));
        assert!(!d.limits.is_empty());
        assert!(!d.regions.is_empty());
        assert!(d.kernel.sections.iter().any(|(n, f)| n == "Privileges & sandboxing" && !f.is_empty()));
    }

    /// Read in the `limited` demo pod on kind (500m, 256Mi, a busy loop).
    #[test]
    fn cgroup_files_from_kind() {
        let stat = "usage_usec 9275512500\nuser_usec 9260235663\nsystem_usec 15276836\nnice_usec 0\nnr_periods 185714\nnr_throttled 184085\nthrottled_usec 4883032854\nnr_bursts 0\nburst_usec 0\n";
        let s = cgroup_cpu_stat(stat);
        assert_eq!(
            (s.usage_usec, s.nr_periods, s.nr_throttled, s.throttled_usec),
            (9275512500, 185714, 184085, 4883032854)
        );
        assert_eq!(cgroup_cpu_max("50000 100000\n"), Some(0.5));
        assert_eq!(cgroup_cpu_max("max 100000\n"), None);
        assert_eq!(cgroup_bytes("268435456\n"), Some(268435456));
        assert_eq!(cgroup_bytes("max\n"), None);
        let events = "low 0\nhigh 0\nmax 0\noom 2\noom_kill 1\noom_group_kill 0\n";
        assert_eq!(cgroup_oom_kills(events), Some(1));
    }
}
