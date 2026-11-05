//! macOS backend. There is no /proc: the kernel hands out process data through
//! `proc_pidinfo(2)` and friends (libproc), `sysctl(3)` and Mach host calls.
//! Each call fills a C struct from `<sys/proc_info.h>`; the field names in the
//! `source` strings below are the names in that header.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::mem::{MaybeUninit, size_of};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ptr::null_mut;

use anyhow::bail;
use libc::{c_char, c_int, c_void};

use super::{Collector, UserCache};
use crate::fmt;
use crate::model::*;

// Not exported by the `libc` crate. Values and offsets checked with offsetof()
// against the macOS SDK's <sys/proc_info.h>.
const PROC_PIDLISTTHREADS: c_int = 6;
const PROC_PIDREGIONPATHINFO: c_int = 8;
const PROC_PIDFDVNODEPATHINFO: c_int = 2;
const PROC_PIDFDSOCKETINFO: c_int = 3;

const VNODE_FDINFOWITHPATH_SIZE: usize = 1200;
const VNODE_MODE: usize = 24 + 4; // pfi (24) + vinfo_stat.vst_mode
const VNODE_PATH: usize = 176;

const REGIONWITHPATH_SIZE: usize = 1272;
const PRI_PROTECTION: usize = 0;
const PRI_MAX_PROTECTION: usize = 4;
const PRI_USER_TAG: usize = 32;
const PRI_PAGES_RESIDENT: usize = 36;
const PRI_SHARE_MODE: usize = 60;
const PRI_ADDRESS: usize = 80;
const PRI_SIZE: usize = 88;
const PRI_PATH: usize = 248;

const SOCKET_FDINFO_SIZE: usize = 792;
const SOI_TYPE: usize = 176;
const SOI_FAMILY: usize = 184;
const SOI_KIND: usize = 256;
const INSI_FPORT: usize = 264;
const INSI_LPORT: usize = 268;
const UNSI_PATH: usize = 280 + 2; // sockaddr_un.sun_path
const INSI_VFLAG: usize = 288;
const INSI_FADDR: usize = 296;
const INSI_LADDR: usize = 312;
const TCPSI_STATE: usize = 344;
const INI_IPV4: u8 = 1;
const SOCKINFO_IN: i32 = 1;
const SOCKINFO_TCP: i32 = 2;

// struct kinfo_proc from <sys/sysctl.h>, what ps(1) reads. Unlike
// proc_pidinfo it is readable for every process, root's included.
const KINFO_PROC_SIZE: usize = 648;
const KP_STARTTIME: usize = 0;
const KP_FLAG: usize = 32;
const KP_STAT: usize = 36;
const KP_PID: usize = 40;
const KP_NICE: usize = 242;
const KP_COMM: usize = 243;
const KP_RUID: usize = 392;
const KP_SVUID: usize = 396;
const KP_UID: usize = 420;
const KP_PPID: usize = 560;
const KP_PGID: usize = 564;
const KP_TDEV: usize = 572;

pub struct MacCollector {
    users: UserCache,
    /// Mach absolute time -> nanoseconds is `ticks * numer / denom`.
    timebase: (u32, u32),
    argmax: usize,
    page_size: u64,
    cmdlines: HashMap<(Pid, u64), String>,
    cpu: CpuInfo,
    volumes: super::space::Cache,
}

impl MacCollector {
    #[allow(deprecated)] // libc points at the mach2 crate; this one call isn't worth a dependency.
    pub fn new() -> Self {
        let mut tb = libc::mach_timebase_info { numer: 1, denom: 1 };
        unsafe { libc::mach_timebase_info(&mut tb) };
        let argmax = sysctl_mib::<c_int>(&[libc::CTL_KERN, libc::KERN_ARGMAX]).unwrap_or(1 << 20);
        MacCollector {
            users: UserCache::default(),
            timebase: (tb.numer, tb.denom.max(1)),
            argmax: argmax as usize,
            page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64,
            cmdlines: HashMap::new(),
            volumes: super::space::Cache::new(),
            cpu: CpuInfo {
                model: sysctl_string("machdep.cpu.brand_string").unwrap_or_default(),
                kinds: super::iokit::core_kinds(),
                mhz: None,
            },
        }
    }

    fn abs_to_ns(&self, ticks: u64) -> u64 {
        (ticks as u128 * self.timebase.0 as u128 / self.timebase.1 as u128) as u64
    }

    fn system(&self) -> SystemInfo {
        let mut load = [0f64; 3];
        unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
        let mem_total = sysctl_name::<u64>("hw.memsize").unwrap_or(0);
        let vm = vm_stats();
        let mem_used = vm
            .map(|v| {
                // Activity Monitor's "Memory Used": app memory + wired + compressed.
                let app = v.internal_page_count.saturating_sub(v.purgeable_count) as u64;
                (app + v.wire_count as u64 + v.compressor_page_count as u64) * self.page_size
            })
            .unwrap_or(0);
        let swap = sysctl_name::<libc::xsw_usage>("vm.swapusage");
        let swap_io = vm.map(|v| IoCounters {
            read: v.swapins * self.page_size,
            write: v.swapouts * self.page_size,
            source: "host_statistics64(HOST_VM_INFO64) swapins, swapouts".into(),
        });
        let boot = sysctl_name::<libc::timeval>("kern.boottime").map_or(0, |t| t.tv_sec as u64);
        SystemInfo {
            load,
            mem_total,
            mem_used,
            swap_total: swap.map_or(0, |s| s.xsu_total),
            swap_used: swap.map_or(0, |s| s.xsu_used),
            uptime_secs: fmt::now_unix().saturating_sub(boot),
            cores: core_ticks(),
            net: net_counters(),
            disk: super::iokit::disk(),
            gpu: super::iokit::gpu(),
            cpu: self.cpu.clone(),
            battery: super::iokit::battery(),
            thermal: super::iokit::thermal(),
            volumes: None,
            swap_io,
            pressure: sysctl_name::<c_int>("kern.memorystatus_vm_pressure_level").map(|l| Pressure::Level(l as u32)),
        }
    }

    fn cmdline(&mut self, pid: Pid, start: u64, fallback: &str) -> String {
        if let Some(c) = self.cmdlines.get(&(pid, start)) {
            return c.clone();
        }
        let cmd = procargs(pid, self.argmax)
            .map(|b| parse_procargs(&b).1.join(" "))
            .filter(|c| !c.is_empty())
            .or_else(|| pidpath(pid))
            .unwrap_or_else(|| fallback.to_string());
        self.cmdlines.insert((pid, start), cmd.clone());
        cmd
    }
}

impl Collector for MacCollector {
    fn platform(&self) -> &'static str {
        "macOS"
    }

    fn snapshot(&mut self) -> Snapshot {
        let pids = all_pids();
        let mut processes = Vec::with_capacity(pids.len());
        for pid in pids {
            // A process can exit between listing and querying it.
            let Some((bsd, _)) = bsd_info(pid) else { continue };
            let task = pidinfo::<libc::proc_taskinfo>(pid, libc::PROC_PIDTASKINFO, 0);
            let name = bsd_name(&bsd);
            let start = bsd.pbi_start_tvsec;
            let cmdline = if bsd.pbi_status == libc::SZOMB { String::new() } else { self.cmdline(pid, start, &name) };
            processes.push(Process {
                space: 0,
                pid,
                ppid: bsd.pbi_ppid as Pid,
                pgid: bsd.pbi_pgid as Pid,
                uid: bsd.pbi_uid,
                user: self.users.name(bsd.pbi_uid),
                name,
                cmdline,
                state: state(&bsd, task.as_ref()),
                nice: bsd.pbi_nice,
                priority: task.map_or(0, |t| t.pti_priority),
                cpu_time_ns: task.map_or(0, |t| self.abs_to_ns(t.pti_total_user + t.pti_total_system)),
                cpu_percent: 0.0,
                rss: task.map_or(0, |t| t.pti_resident_size),
                vsz: task.map_or(0, |t| t.pti_virtual_size),
                threads: task.map_or(0, |t| t.pti_threadnum as u32),
                fds: task.and_then(|_| list_fds(pid)).map(|f| f.len() as u32),
                start_time: start,
                restricted: task.is_none() && bsd.pbi_status != libc::SZOMB,
                origin: None,
            });
        }
        let live: std::collections::HashSet<_> = processes.iter().map(|p| (p.pid, p.start_time)).collect();
        self.cmdlines.retain(|k, _| live.contains(k));
        let system = SystemInfo { volumes: self.volumes.get(volumes), ..self.system() };
        Snapshot { system, processes, clock: None, containers: Vec::new() }
    }

    fn details(&mut self, id: ProcId) -> anyhow::Result<ProcessDetails> {
        let pid = id.pid;
        let Some((bsd, from_kinfo)) = bsd_info(pid) else {
            bail!("neither proc_pidinfo nor sysctl(KERN_PROC_PID) know {pid}: the process is gone");
        };
        let task = pidinfo::<libc::proc_taskinfo>(pid, libc::PROC_PIDTASKINFO, 0);
        let rusage = rusage_v4(pid);
        let args = procargs(pid, self.argmax);
        let parsed = args.as_deref().map(parse_procargs);
        let mut d = ProcessDetails { pid, name: bsd_name(&bsd), ..Default::default() };
        if from_kinfo {
            d.notes.push(
                "proc_pidinfo(PROC_PIDTBSDINFO) is denied for other users' processes; identity fields \
                 come from sysctl(KERN_PROC_PID) → struct kinfo_proc instead, which is what ps(1) uses."
                    .into(),
            );
        }
        if task.is_none() {
            d.notes.push(
                "PROC_PIDTASKINFO returned EPERM: macOS only reveals task data (memory, CPU, \
                 fds, threads) for your own processes unless you run as root."
                    .into(),
            );
        }
        if args.is_none() {
            d.notes.push("sysctl(KERN_PROCARGS2) denied: argv/env are private to the owner.".into());
        }

        d.overview = self.overview(&bsd, from_kinfo, task.as_ref(), parsed.as_ref());
        d.memory = self.memory(task.as_ref(), rusage.as_ref());
        if task.is_some() {
            d.regions = regions(pid, self.page_size);
            d.files = files(pid);
            d.threads = threads(pid);
        }
        if let Some((_, _, env)) = &parsed {
            d.env = env
                .iter()
                .map(|e| match e.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (e.clone(), String::new()),
                })
                .collect();
        }
        d.limits = limits(pid);
        d.kernel = kernel(&bsd, task.as_ref(), rusage.as_ref(), self);
        d.raw = raw(pid, &bsd, from_kinfo, task.as_ref(), rusage.as_ref(), args.as_deref());
        Ok(d)
    }
}

impl MacCollector {
    fn overview(
        &mut self,
        bsd: &libc::proc_bsdinfo,
        from_kinfo: bool,
        task: Option<&libc::proc_taskinfo>,
        args: Option<&(String, Vec<String>, Vec<String>)>,
    ) -> Vec<Field> {
        let bsd_src = if from_kinfo {
            "sysctl(KERN_PROC_PID) → kinfo_proc ≈ proc_bsdinfo"
        } else {
            "proc_pidinfo(PROC_PIDTBSDINFO) → proc_bsdinfo"
        };
        const TASK: &str = "proc_pidinfo(PROC_PIDTASKINFO) → proc_taskinfo";
        let pid = bsd.pbi_pid as Pid;
        let mut f = vec![
            Field::new("pid", "PID", bsd.pbi_pid, format!("{bsd_src}.pbi_pid")).hint("pbi_pid"),
            Field::new("ppid", "Parent PID", bsd.pbi_ppid, format!("{bsd_src}.pbi_ppid")).hint("pbi_ppid"),
            Field::new("pgid", "Process group", bsd.pbi_pgid, format!("{bsd_src}.pbi_pgid")).hint("pbi_pgid"),
            Field::new(
                "uid",
                "User (effective)",
                format!("{} ({})", self.users.name(bsd.pbi_uid), bsd.pbi_uid),
                format!("{bsd_src}.pbi_uid"),
            )
            .hint("pbi_uid"),
            Field::new(
                "uid",
                "Real / saved UID",
                format!("{} / {}", bsd.pbi_ruid, bsd.pbi_svuid),
                format!("{bsd_src}.pbi_ruid, pbi_svuid"),
            )
            .hint("pbi_ruid"),
            Field::new("comm", "Name", bsd_name(bsd), format!("{bsd_src}.pbi_name / pbi_comm")).hint("pbi_name"),
            Field::new("exe", "Executable", pidpath(pid).unwrap_or_else(|| "—".into()), "proc_pidpath(pid)"),
            Field::new(
                "cmdline",
                "Command line",
                args.map_or("— (not permitted)".into(), |a| a.1.join(" ")),
                "sysctl(CTL_KERN, KERN_PROCARGS2, pid) → argv",
            )
            .hint("argv"),
        ];
        let st = state(bsd, task);
        f.push(
            Field::new(
                "state",
                "State",
                format!("{} — {}", st.letter(), st.label()),
                format!(
                    "{bsd_src}.pbi_status = {} ({}), refined by {TASK}.pti_numrunning",
                    bsd.pbi_status,
                    p_stat_name(bsd.pbi_status)
                ),
            )
            .hint("pbi_status"),
        );
        f.push(Field::new("nice", "Nice", bsd.pbi_nice, format!("{bsd_src}.pbi_nice")).hint("pbi_nice"));
        f.push(
            Field::new(
                "start_time",
                "Started",
                format!("{} ({})", bsd.pbi_start_tvsec, fmt::age(bsd.pbi_start_tvsec)),
                format!("{bsd_src}.pbi_start_tvsec"),
            )
            .hint("pbi_start_tvsec"),
        );
        f.push(
            Field::new(
                "tty",
                "Controlling TTY",
                if bsd.e_tdev == u32::MAX { "none".to_string() } else { format!("dev 0x{:x}", bsd.e_tdev) },
                format!("{bsd_src}.e_tdev"),
            )
            .hint("e_tdev"),
        );
        if let Some(t) = task {
            f.extend([
                Field::new("priority", "Priority", t.pti_priority, format!("{TASK}.pti_priority")).hint("pti_priority"),
                Field::new("threads", "Threads", t.pti_threadnum, format!("{TASK}.pti_threadnum"))
                    .hint("pti_threadnum"),
                Field::new("threads", "Threads running now", t.pti_numrunning, format!("{TASK}.pti_numrunning"))
                    .hint("pti_numrunning"),
                Field::new(
                    "cpu_time",
                    "CPU time user / system",
                    format!(
                        "{} / {}",
                        fmt::cpu_time(self.abs_to_ns(t.pti_total_user)),
                        fmt::cpu_time(self.abs_to_ns(t.pti_total_system))
                    ),
                    format!(
                        "{TASK}.pti_total_user / pti_total_system (Mach ticks × {}/{})",
                        self.timebase.0, self.timebase.1
                    ),
                )
                .hint("pti_total_user"),
                Field::new("ctx_switches", "Context switches", t.pti_csw, format!("{TASK}.pti_csw")).hint("pti_csw"),
                Field::new(
                    "syscalls",
                    "Syscalls BSD / Mach",
                    format!("{} / {}", counter(t.pti_syscalls_unix), counter(t.pti_syscalls_mach)),
                    format!("{TASK}.pti_syscalls_unix / pti_syscalls_mach"),
                )
                .hint("pti_syscalls_unix"),
                Field::new(
                    "mach_msgs",
                    "Mach messages sent / received",
                    format!("{} / {}", t.pti_messages_sent, t.pti_messages_received),
                    format!("{TASK}.pti_messages_sent / pti_messages_received"),
                )
                .hint("pti_messages_sent"),
            ]);
        }
        f
    }

    fn memory(&self, task: Option<&libc::proc_taskinfo>, ru: Option<&libc::rusage_info_v4>) -> Vec<Field> {
        const TASK: &str = "proc_pidinfo(PROC_PIDTASKINFO) → proc_taskinfo";
        const RU: &str = "proc_pid_rusage(RUSAGE_INFO_V4) → rusage_info_v4";
        let mut f = Vec::new();
        if let Some(t) = task {
            f.extend([
                Field::new(
                    "vsz",
                    "Virtual size (VSZ)",
                    fmt::bytes_exact(t.pti_virtual_size),
                    format!("{TASK}.pti_virtual_size"),
                )
                .hint("pti_virtual_size"),
                Field::new(
                    "rss",
                    "Resident (RSS)",
                    fmt::bytes_exact(t.pti_resident_size),
                    format!("{TASK}.pti_resident_size"),
                )
                .hint("pti_resident_size"),
                Field::new("faults", "Page faults", t.pti_faults, format!("{TASK}.pti_faults")).hint("pti_faults"),
                Field::new("pageins", "Page-ins", t.pti_pageins, format!("{TASK}.pti_pageins")).hint("pti_pageins"),
                Field::new("cow_faults", "Copy-on-write faults", t.pti_cow_faults, format!("{TASK}.pti_cow_faults"))
                    .hint("pti_cow_faults"),
            ]);
        }
        if let Some(r) = ru {
            f.extend([
                Field::new(
                    "phys_footprint",
                    "Physical footprint",
                    fmt::bytes_exact(r.ri_phys_footprint),
                    format!("{RU}.ri_phys_footprint"),
                )
                .hint("ri_phys_footprint"),
                Field::new(
                    "phys_footprint",
                    "Peak footprint (lifetime)",
                    fmt::bytes_exact(r.ri_lifetime_max_phys_footprint),
                    format!("{RU}.ri_lifetime_max_phys_footprint"),
                )
                .hint("ri_lifetime_max_phys_footprint"),
                Field::new("wired", "Wired", fmt::bytes_exact(r.ri_wired_size), format!("{RU}.ri_wired_size"))
                    .hint("ri_wired_size"),
                Field::new(
                    "io_read",
                    "Disk bytes read",
                    fmt::bytes_exact(r.ri_diskio_bytesread),
                    format!("{RU}.ri_diskio_bytesread"),
                )
                .hint("ri_diskio_bytesread"),
                Field::new(
                    "io_write",
                    "Disk bytes written",
                    fmt::bytes_exact(r.ri_diskio_byteswritten),
                    format!("{RU}.ri_diskio_byteswritten"),
                )
                .hint("ri_diskio_byteswritten"),
                Field::new(
                    "wakeups",
                    "Wakeups idle / interrupt",
                    format!("{} / {}", r.ri_pkg_idle_wkups, r.ri_interrupt_wkups),
                    format!("{RU}.ri_pkg_idle_wkups / ri_interrupt_wkups"),
                )
                .hint("ri_pkg_idle_wkups"),
                Field::new(
                    "instructions",
                    "Instructions / cycles",
                    format!(
                        "{} / {} (IPC {:.2})",
                        r.ri_instructions,
                        r.ri_cycles,
                        r.ri_instructions as f64 / r.ri_cycles.max(1) as f64
                    ),
                    format!("{RU}.ri_instructions / ri_cycles"),
                )
                .hint("ri_instructions"),
            ]);
        }
        f
    }
}

fn kernel(
    bsd: &libc::proc_bsdinfo,
    task: Option<&libc::proc_taskinfo>,
    ru: Option<&libc::rusage_info_v4>,
    c: &MacCollector,
) -> KernelView {
    const BSD: &str = "proc_bsdinfo";
    let mut sections = vec![(
        "Process flags".to_string(),
        PROC_FLAGS
            .iter()
            .map(|(bit, name, what)| {
                Field::new(
                    "proc_flags",
                    *name,
                    if bsd.pbi_flags & bit != 0 { format!("yes — {what}") } else { "no".into() },
                    format!("{BSD}.pbi_flags & 0x{bit:x}"),
                )
                .hint("pbi_flags")
            })
            .collect(),
    )];
    if let Some(t) = task {
        sections.push((
            "Scheduling".to_string(),
            vec![
                Field::new("policy", "Policy", policy_name(t.pti_policy), "proc_taskinfo.pti_policy")
                    .hint("pti_policy"),
                Field::new("priority", "Priority", t.pti_priority, "proc_taskinfo.pti_priority").hint("pti_priority"),
            ],
        ));
    }
    if let Some(r) = ru {
        let qos = [
            ("user-interactive", r.ri_cpu_time_qos_user_interactive),
            ("user-initiated", r.ri_cpu_time_qos_user_initiated),
            ("default", r.ri_cpu_time_qos_default),
            ("utility", r.ri_cpu_time_qos_utility),
            ("background", r.ri_cpu_time_qos_background),
            ("maintenance", r.ri_cpu_time_qos_maintenance),
            ("legacy", r.ri_cpu_time_qos_legacy),
        ];
        sections.push((
            "CPU time by QoS class".to_string(),
            qos.iter()
                .map(|(n, v)| {
                    Field::new(
                        "qos",
                        *n,
                        fmt::cpu_time(c.abs_to_ns(*v)),
                        format!("rusage_info_v4.ri_cpu_time_qos_{}", n.replace('-', "_")),
                    )
                    .hint("ri_cpu_time_qos")
                })
                .collect(),
        ));
    }
    KernelView {
        sections,
        note: Some(
            "macOS has no namespaces, cgroups, Linux capabilities or seccomp. Rough equivalents: \
             containers run inside a Linux VM; resource control uses coalitions, QoS classes and \
             jetsam; privileges come from entitlements, TCC and code signing; syscall filtering is \
             the App Sandbox (sandbox-exec profiles). Run rtop in the Linux container to see the \
             real kernel mechanisms."
                .into(),
        ),
    }
}

const PROC_FLAGS: &[(u32, &str, &str)] = &[
    (0x1, "SYSTEM", "system process"),
    (0x2, "TRACED", "being traced (debugger attached)"),
    (0x4, "INEXIT", "working its way through exit()"),
    (0x8, "PPWAIT", "parent is waiting in vfork()"),
    (0x10, "LP64", "64-bit process"),
    (0x20, "SLEADER", "session leader"),
    (0x40, "CTTY", "has a controlling terminal"),
    (0x2000, "PSUGID", "changed privileges since exec (set-id)"),
    (0x4000, "EXEC", "has called exec()"),
];

fn limits(pid: Pid) -> Vec<Field> {
    let mut f = Vec::new();
    if pid == std::process::id() as Pid {
        for (key, label, res) in [
            ("limit_nofile", "Open files (RLIMIT_NOFILE)", libc::RLIMIT_NOFILE),
            ("limits", "Processes (RLIMIT_NPROC)", libc::RLIMIT_NPROC),
            ("limits", "Stack (RLIMIT_STACK)", libc::RLIMIT_STACK),
            ("limits", "Core file (RLIMIT_CORE)", libc::RLIMIT_CORE),
        ] {
            let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if unsafe { libc::getrlimit(res, &mut rl) } == 0 {
                f.push(Field::new(
                    key,
                    label,
                    format!("soft {} / hard {}", rlim(rl.rlim_cur), rlim(rl.rlim_max)),
                    "getrlimit(2) on self",
                ));
            }
        }
    } else {
        f.push(Field::new(
            "limits",
            "Per-process rlimits",
            "not readable for other processes on macOS",
            "Linux exposes them in /proc/<pid>/limits; macOS only has getrlimit(2) on yourself",
        ));
    }
    for (label, name) in [
        ("kern.maxfiles (system-wide fds)", "kern.maxfiles"),
        ("kern.maxfilesperproc", "kern.maxfilesperproc"),
        ("kern.maxproc (system-wide)", "kern.maxproc"),
        ("kern.maxprocperuid", "kern.maxprocperuid"),
    ] {
        if let Some(v) = sysctl_name::<c_int>(name) {
            f.push(Field::new("sysctl_limits", label, v, format!("sysctlbyname(\"{name}\")")));
        }
    }
    f
}

fn rlim(v: libc::rlim_t) -> String {
    if v == libc::RLIM_INFINITY { "unlimited".into() } else { v.to_string() }
}

fn raw(
    pid: Pid,
    bsd: &libc::proc_bsdinfo,
    from_kinfo: bool,
    task: Option<&libc::proc_taskinfo>,
    ru: Option<&libc::rusage_info_v4>,
    args: Option<&[u8]>,
) -> Vec<RawSource> {
    macro_rules! dump {
        ($s:expr; $($f:ident),* $(,)?) => {{
            let mut out = String::new();
            $( out.push_str(&format!("{:<34} = {}\n", stringify!($f), $s.$f)); )*
            out
        }};
    }
    let mut v = vec![RawSource {
        title: if from_kinfo {
            format!(
                "sysctl(CTL_KERN, KERN_PROC, KERN_PROC_PID, {pid}) → struct kinfo_proc, shown as proc_bsdinfo fields"
            )
        } else {
            format!("proc_pidinfo({pid}, PROC_PIDTBSDINFO) → struct proc_bsdinfo")
        },
        body: dump!(bsd; pbi_flags, pbi_status, pbi_xstatus, pbi_pid, pbi_ppid, pbi_uid, pbi_gid, pbi_ruid,
            pbi_rgid, pbi_svuid, pbi_svgid, pbi_nfiles, pbi_pgid, pbi_pjobc, e_tdev, e_tpgid, pbi_nice,
            pbi_start_tvsec, pbi_start_tvusec)
            + &format!(
                "{:<34} = {:?}\n{:<34} = {:?}\n",
                "pbi_comm",
                cstr(&bsd.pbi_comm),
                "pbi_name",
                cstr(&bsd.pbi_name)
            ),
    }];
    if let Some(t) = task {
        v.push(RawSource {
            title: format!("proc_pidinfo({pid}, PROC_PIDTASKINFO) → struct proc_taskinfo"),
            body: dump!(t; pti_virtual_size, pti_resident_size, pti_total_user, pti_total_system,
                pti_threads_user, pti_threads_system, pti_policy, pti_faults, pti_pageins, pti_cow_faults,
                pti_messages_sent, pti_messages_received, pti_syscalls_mach, pti_syscalls_unix, pti_csw,
                pti_threadnum, pti_numrunning, pti_priority),
        });
    }
    if let Some(r) = ru {
        v.push(RawSource {
            title: format!("proc_pid_rusage({pid}, RUSAGE_INFO_V4) → struct rusage_info_v4"),
            body: dump!(r; ri_user_time, ri_system_time, ri_pkg_idle_wkups, ri_interrupt_wkups, ri_pageins,
                ri_wired_size, ri_resident_size, ri_phys_footprint, ri_proc_start_abstime,
                ri_diskio_bytesread, ri_diskio_byteswritten, ri_cpu_time_qos_default,
                ri_cpu_time_qos_maintenance, ri_cpu_time_qos_background, ri_cpu_time_qos_utility,
                ri_cpu_time_qos_legacy, ri_cpu_time_qos_user_initiated, ri_cpu_time_qos_user_interactive,
                ri_logical_writes, ri_lifetime_max_phys_footprint, ri_instructions, ri_cycles,
                ri_interval_max_phys_footprint, ri_runnable_time),
        });
    }
    if let Some(b) = args {
        let (exec, argv, env) = parse_procargs(b);
        let mut body = format!(
            "Layout: int argc | exec_path\\0 | padding\\0… | argv[0]\\0 … argv[argc-1]\\0 | env\\0 … | \\0\n\n\
             argc      = {}\nexec_path = {exec:?}\n",
            argv.len()
        );
        for (i, a) in argv.iter().enumerate() {
            body.push_str(&format!("argv[{i}]   = {a:?}\n"));
        }
        body.push_str(&format!("env       = {} variables (see Env tab)\n\nFirst bytes:\n", env.len()));
        body.push_str(&hexdump(&b[..b.len().min(256)]));
        v.push(RawSource { title: format!("sysctl(CTL_KERN, KERN_PROCARGS2, {pid}) — {} bytes", b.len()), body });
    }
    if let Some(fds) = list_fds(pid) {
        let body = fds
            .iter()
            .map(|f| {
                format!(
                    "proc_fd = {:<5} proc_fdtype = {} ({})\n",
                    f.proc_fd,
                    f.proc_fdtype,
                    fd_type_name(f.proc_fdtype)
                )
            })
            .collect();
        v.push(RawSource {
            title: format!("proc_pidinfo({pid}, PROC_PIDLISTFDS) → struct proc_fdinfo[{}]", fds.len()),
            body,
        });
    }
    v
}

/// proc_taskinfo counters are 32-bit and stick at INT32_MAX instead of wrapping.
fn counter(v: i32) -> String {
    if v == i32::MAX { format!("≥{v} (32-bit counter saturated)") } else { v.to_string() }
}

fn hexdump(b: &[u8]) -> String {
    b.chunks(16)
        .enumerate()
        .map(|(i, c)| {
            let hex: Vec<String> = c.iter().map(|x| format!("{x:02x}")).collect();
            let txt: String = c
                .iter()
                .map(|&x| match x {
                    0 => '␀',
                    0x20..=0x7e => x as char,
                    _ => '.',
                })
                .collect();
            format!("{:04x}  {:<48} {txt}\n", i * 16, hex.join(" "))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Thin unsafe wrappers. Each returns None when the kernel says no.

fn pidinfo<T>(pid: Pid, flavor: c_int, arg: u64) -> Option<T> {
    let mut v = MaybeUninit::<T>::zeroed();
    let size = size_of::<T>() as c_int;
    let n = unsafe { libc::proc_pidinfo(pid, flavor, arg, v.as_mut_ptr().cast(), size) };
    // SAFETY: the kernel filled exactly `size` bytes of a plain-old-data struct.
    (n == size).then(|| unsafe { v.assume_init() })
}

/// proc_bsdinfo for `pid`, and whether it had to be rebuilt from kinfo_proc.
fn bsd_info(pid: Pid) -> Option<(libc::proc_bsdinfo, bool)> {
    pidinfo::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO, 0)
        .map(|b| (b, false))
        .or_else(|| kinfo_proc(pid).map(|b| (b, true)))
}

fn kinfo_proc(pid: Pid) -> Option<libc::proc_bsdinfo> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut buf = vec![0u8; KINFO_PROC_SIZE];
    let mut len = KINFO_PROC_SIZE;
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 4, buf.as_mut_ptr().cast(), &mut len, null_mut(), 0) };
    if rc != 0 || len != KINFO_PROC_SIZE {
        return None;
    }
    // SAFETY: proc_bsdinfo is plain old data; all-zero is a valid value.
    let mut b: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    b.pbi_pid = rd_u32(&buf, KP_PID);
    b.pbi_ppid = rd_u32(&buf, KP_PPID);
    b.pbi_pgid = rd_u32(&buf, KP_PGID);
    b.pbi_uid = rd_u32(&buf, KP_UID);
    b.pbi_ruid = rd_u32(&buf, KP_RUID);
    b.pbi_svuid = rd_u32(&buf, KP_SVUID);
    b.pbi_status = buf[KP_STAT] as u32;
    b.pbi_nice = buf[KP_NICE] as i8 as i32;
    b.pbi_start_tvsec = rd_u64(&buf, KP_STARTTIME);
    b.e_tdev = rd_u32(&buf, KP_TDEV);
    for (dst, &src) in b.pbi_comm.iter_mut().zip(&buf[KP_COMM..KP_COMM + libc::MAXCOMLEN]) {
        *dst = src as c_char;
    }
    // p_flag uses P_* bits; translate the ones PROC_FLAGS knows about.
    let p_flag = rd_u32(&buf, KP_FLAG);
    for (p_bit, proc_bit) in [(0x200, 0x1), (0x800, 0x2), (0x4, 0x10), (0x2, 0x80), (0x100, 0x2000), (0x4000, 0x4000)] {
        if p_flag & p_bit != 0 {
            b.pbi_flags |= proc_bit;
        }
    }
    Some(b)
}

fn all_pids() -> Vec<Pid> {
    let n = unsafe { libc::proc_listallpids(null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0 as Pid; n as usize + 64];
    let bytes = (buf.len() * size_of::<Pid>()) as c_int;
    let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
    buf.truncate(n.max(0) as usize);
    buf.retain(|&p| p >= 0);
    buf
}

fn pidpath(pid: Pid) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

fn rusage_v4(pid: Pid) -> Option<libc::rusage_info_v4> {
    let mut v = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let rc = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, v.as_mut_ptr().cast()) };
    (rc == 0).then(|| unsafe { v.assume_init() })
}

fn procargs(pid: Pid, argmax: usize) -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut buf = vec![0u8; argmax];
    let mut size = argmax;
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, null_mut(), 0) };
    (rc == 0 && size >= 4).then(|| {
        buf.truncate(size);
        buf
    })
}

/// Splits a KERN_PROCARGS2 buffer into (exec path, argv, env).
pub fn parse_procargs(buf: &[u8]) -> (String, Vec<String>, Vec<String>) {
    if buf.len() < 4 {
        return Default::default();
    }
    let argc = i32::from_ne_bytes(buf[..4].try_into().unwrap()).max(0) as usize;
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let exec = String::from_utf8_lossy(&rest[..exec_end]).into_owned();
    let mut strings = rest[exec_end..]
        .split(|&b| b == 0)
        .skip_while(|s| s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned());
    let argv: Vec<String> = strings.by_ref().take(argc).collect();
    let env = strings.take_while(|s| !s.is_empty()).collect();
    (exec, argv, env)
}

fn sysctl_mib<T>(mib: &[c_int]) -> Option<T> {
    let mut v = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>();
    let mut mib = mib.to_vec();
    let rc =
        unsafe { libc::sysctl(mib.as_mut_ptr(), mib.len() as u32, v.as_mut_ptr().cast(), &mut size, null_mut(), 0) };
    (rc == 0 && size == size_of::<T>()).then(|| unsafe { v.assume_init() })
}

fn sysctl_name<T>(name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    let mut v = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>();
    let rc = unsafe { libc::sysctlbyname(cname.as_ptr(), v.as_mut_ptr().cast(), &mut size, null_mut(), 0) };
    (rc == 0 && size == size_of::<T>()).then(|| unsafe { v.assume_init() })
}

fn sysctl_string(name: &str) -> Option<String> {
    let name = CString::new(name).ok()?;
    let mut len = 0;
    if unsafe { libc::sysctlbyname(name.as_ptr(), null_mut(), &mut len, null_mut(), 0) } != 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    if unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, null_mut(), 0) } != 0 {
        return None;
    }
    Some(cstr_bytes(&buf))
}

#[allow(deprecated)]
fn vm_stats() -> Option<libc::vm_statistics64> {
    let mut v = MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let kr = unsafe {
        libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, v.as_mut_ptr().cast(), &mut count)
    };
    (kr == libc::KERN_SUCCESS).then(|| unsafe { v.assume_init() })
}

#[allow(deprecated)]
fn core_ticks() -> Vec<CpuTicks> {
    let mut ncpu: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = null_mut();
    let mut count: libc::mach_msg_type_number_t = 0;
    let kr = unsafe {
        libc::host_processor_info(
            libc::mach_host_self(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut ncpu,
            &mut info,
            &mut count,
        )
    };
    if kr != libc::KERN_SUCCESS || info.is_null() {
        return Vec::new();
    }
    // SAFETY: the kernel returned `count` integers; we free them right after copying.
    let ticks = unsafe { std::slice::from_raw_parts(info, count as usize) };
    let cores = ticks
        .as_chunks::<{ libc::CPU_STATE_MAX as usize }>()
        .0
        .iter()
        .take(ncpu as usize)
        .map(|c| {
            let t = |i: c_int| c[i as usize] as u32 as u64;
            let busy = t(libc::CPU_STATE_USER) + t(libc::CPU_STATE_SYSTEM) + t(libc::CPU_STATE_NICE);
            CpuTicks { busy, total: busy + t(libc::CPU_STATE_IDLE) }
        })
        .collect();
    unsafe {
        libc::vm_deallocate(
            libc::mach_task_self(),
            info as libc::vm_address_t,
            (count as usize * size_of::<libc::integer_t>()) as libc::vm_size_t,
        );
    }
    cores
}

/// Bytes in and out of the `en*` interfaces (Wi-Fi, Ethernet). Tunnels,
/// bridges and VM interfaces are skipped: their traffic also crosses an `en*`
/// interface, so adding them would count it twice. NET_RT_IFLIST2 gives the
/// 64-bit counters; getifaddrs' `if_data` is 32-bit and wraps at 4 GiB.
fn net_counters() -> Option<IoCounters> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut len = 0;
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 6, null_mut(), &mut len, null_mut(), 0) } != 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 6, buf.as_mut_ptr().cast(), &mut len, null_mut(), 0) } != 0 {
        return None;
    }
    let mut ifaces = Vec::new();
    let mut off = 0;
    while off + size_of::<libc::if_msghdr2>() <= len {
        let msglen = rd_u16(&buf, off) as usize;
        if msglen == 0 {
            break;
        }
        if buf[off + 3] as c_int == libc::RTM_IFINFO2 {
            // SAFETY: an RTM_IFINFO2 message starts with an if_msghdr2.
            let m: libc::if_msghdr2 = unsafe { std::ptr::read_unaligned(buf[off..].as_ptr().cast()) };
            let mut name = [0 as c_char; libc::IF_NAMESIZE];
            if m.ifm_flags & libc::IFF_LOOPBACK == 0
                && !unsafe { libc::if_indextoname(m.ifm_index as u32, name.as_mut_ptr()) }.is_null()
            {
                let d = m.ifm_data;
                ifaces.push((cstr(&name), d.ifi_ibytes, d.ifi_obytes));
            }
        }
        off += msglen;
    }
    let physical: Vec<_> = ifaces.iter().filter(|(n, ..)| n.starts_with("en")).collect();
    let used = if physical.is_empty() { ifaces.iter().collect() } else { physical };
    let mut c = IoCounters::default();
    let mut names = Vec::new();
    for (n, rx, tx) in used {
        // Interfaces that never moved a byte (spare Thunderbolt ports) are noise.
        if rx + tx > 0 {
            c.read += rx;
            c.write += tx;
            names.push(n.as_str());
        }
    }
    c.source = format!("{} · NET_RT_IFLIST2", names.join(" "));
    Some(c)
}

/// Mounted filesystems from getfsstat(2). MNT_NOWAIT returns what the
/// kernel has cached instead of asking each filesystem, so a hung network
/// mount can't block the UI.
fn volumes() -> Option<Volumes> {
    // Not in the libc crate: <sys/mount.h> MNT_DONTBROWSE, "nobrowse".
    const MNT_DONTBROWSE: u32 = 0x0010_0000;
    let n = unsafe { libc::getfsstat(null_mut(), 0, libc::MNT_NOWAIT) };
    if n <= 0 {
        return None;
    }
    // Room for mounts that appear between the two calls.
    let cap = n as usize + 8;
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(cap);
    let got =
        unsafe { libc::getfsstat(buf.as_mut_ptr(), (cap * size_of::<libc::statfs>()) as c_int, libc::MNT_NOWAIT) };
    if got <= 0 {
        return None;
    }
    // SAFETY: getfsstat filled `got` entries.
    unsafe { buf.set_len(got as usize) };
    let entries = buf
        .iter()
        .map(|f| super::space::FsEntry {
            mount: cstr(&f.f_mntonname),
            device: cstr(&f.f_mntfromname),
            fstype: cstr(&f.f_fstypename),
            blocks: f.f_blocks,
            bfree: f.f_bfree,
            bavail: f.f_bavail,
            bsize: f.f_bsize as u64,
            browse: f.f_flags & MNT_DONTBROWSE == 0,
        })
        .collect();
    Some(Volumes::new(
        super::space::group(entries),
        "getfsstat(MNT_NOWAIT) → statfs f_blocks, f_bfree, f_bavail × f_bsize",
    ))
}

fn list_fds(pid: Pid) -> Option<Vec<libc::proc_fdinfo>> {
    let need = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, null_mut(), 0) };
    if need <= 0 {
        return None;
    }
    let cap = need as usize / size_of::<libc::proc_fdinfo>() + 16;
    let mut buf: Vec<libc::proc_fdinfo> = Vec::with_capacity(cap);
    let bytes = (cap * size_of::<libc::proc_fdinfo>()) as c_int;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, buf.as_mut_ptr().cast(), bytes) };
    if n <= 0 {
        return None;
    }
    // SAFETY: the kernel wrote `n` bytes worth of proc_fdinfo entries.
    unsafe { buf.set_len(n as usize / size_of::<libc::proc_fdinfo>()) };
    Some(buf)
}

fn files(pid: Pid) -> Vec<OpenFile> {
    list_fds(pid)
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let (kind, target) = match f.proc_fdtype as c_int {
                libc::PROX_FDTYPE_VNODE => vnode(pid, f.proc_fd),
                libc::PROX_FDTYPE_SOCKET => socket(pid, f.proc_fd),
                _ => (fd_type_name(f.proc_fdtype).to_string(), String::new()),
            };
            OpenFile { fd: f.proc_fd, kind, target }
        })
        .collect()
}

fn fd_type_name(t: u32) -> &'static str {
    match t as c_int {
        libc::PROX_FDTYPE_VNODE => "vnode",
        libc::PROX_FDTYPE_SOCKET => "socket",
        libc::PROX_FDTYPE_PSHM => "posix shm",
        libc::PROX_FDTYPE_PSEM => "posix sem",
        libc::PROX_FDTYPE_KQUEUE => "kqueue",
        libc::PROX_FDTYPE_PIPE => "pipe",
        libc::PROX_FDTYPE_FSEVENTS => "fsevents",
        libc::PROX_FDTYPE_NETPOLICY => "netpolicy",
        libc::PROX_FDTYPE_CHANNEL => "channel",
        libc::PROX_FDTYPE_NEXUS => "nexus",
        _ => "other",
    }
}

fn fdinfo(pid: Pid, fd: i32, flavor: c_int, size: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; size];
    let n = unsafe { libc::proc_pidfdinfo(pid, fd, flavor, buf.as_mut_ptr().cast::<c_void>(), size as c_int) };
    (n as usize == size).then_some(buf)
}

fn vnode(pid: Pid, fd: i32) -> (String, String) {
    let Some(b) = fdinfo(pid, fd, PROC_PIDFDVNODEPATHINFO, VNODE_FDINFOWITHPATH_SIZE) else {
        return ("vnode".into(), String::new());
    };
    let mode = rd_u16(&b, VNODE_MODE) as libc::mode_t;
    (file_kind(mode).into(), cstr_bytes(&b[VNODE_PATH..]))
}

pub fn file_kind(mode: libc::mode_t) -> &'static str {
    match mode & libc::S_IFMT {
        libc::S_IFREG => "REG",
        libc::S_IFDIR => "DIR",
        libc::S_IFCHR => "CHR",
        libc::S_IFBLK => "BLK",
        libc::S_IFIFO => "FIFO",
        libc::S_IFLNK => "LINK",
        libc::S_IFSOCK => "SOCK",
        _ => "vnode",
    }
}

fn socket(pid: Pid, fd: i32) -> (String, String) {
    let Some(b) = fdinfo(pid, fd, PROC_PIDFDSOCKETINFO, SOCKET_FDINFO_SIZE) else {
        return ("socket".into(), String::new());
    };
    let family = rd_i32(&b, SOI_FAMILY);
    let stype = rd_i32(&b, SOI_TYPE);
    let kind = rd_i32(&b, SOI_KIND);
    match family {
        libc::AF_INET | libc::AF_INET6 if kind == SOCKINFO_IN || kind == SOCKINFO_TCP => {
            let v4 = b[INSI_VFLAG] & INI_IPV4 != 0;
            let addr = |off: usize, port_off: usize| {
                let port = u16::from_be(rd_i32(&b, port_off) as u16);
                let ip = if v4 {
                    Ipv4Addr::new(b[off + 12], b[off + 13], b[off + 14], b[off + 15]).to_string()
                } else {
                    let o: [u8; 16] = b[off..off + 16].try_into().unwrap();
                    format!("[{}]", Ipv6Addr::from(o))
                };
                if port == 0 { format!("{ip}:*") } else { format!("{ip}:{port}") }
            };
            let proto = match (stype, v4) {
                (libc::SOCK_STREAM, true) => "TCP",
                (libc::SOCK_STREAM, false) => "TCP6",
                (libc::SOCK_DGRAM, true) => "UDP",
                (libc::SOCK_DGRAM, false) => "UDP6",
                _ => "INET",
            };
            let mut target = format!("{} → {}", addr(INSI_LADDR, INSI_LPORT), addr(INSI_FADDR, INSI_FPORT));
            if kind == SOCKINFO_TCP {
                target.push_str(&format!("  {}", tcp_state(rd_i32(&b, TCPSI_STATE))));
            }
            (proto.into(), target)
        }
        libc::AF_UNIX => {
            let path = cstr_bytes(&b[UNSI_PATH..UNSI_PATH + 104]);
            ("UNIX".into(), if path.is_empty() { "(unnamed)".into() } else { path })
        }
        libc::AF_SYSTEM => ("SYSTEM".into(), "kernel control / event socket".into()),
        libc::AF_ROUTE => ("ROUTE".into(), "routing socket".into()),
        f => ("socket".into(), format!("family {f}")),
    }
}

fn tcp_state(s: i32) -> &'static str {
    [
        "CLOSED",
        "LISTEN",
        "SYN_SENT",
        "SYN_RECEIVED",
        "ESTABLISHED",
        "CLOSE_WAIT",
        "FIN_WAIT_1",
        "CLOSING",
        "LAST_ACK",
        "FIN_WAIT_2",
        "TIME_WAIT",
    ]
    .get(s as usize)
    .copied()
    .unwrap_or("?")
}

fn regions(pid: Pid, page_size: u64) -> Vec<Region> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; REGIONWITHPATH_SIZE];
    let mut addr = 0u64;
    while out.len() < 5000 {
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDREGIONPATHINFO, addr, buf.as_mut_ptr().cast(), REGIONWITHPATH_SIZE as c_int)
        };
        if n as usize != REGIONWITHPATH_SIZE {
            break;
        }
        let start = rd_u64(&buf, PRI_ADDRESS);
        let size = rd_u64(&buf, PRI_SIZE);
        if size == 0 {
            break;
        }
        let tag = rd_u32(&buf, PRI_USER_TAG);
        let share = rd_u32(&buf, PRI_SHARE_MODE);
        let path = cstr_bytes(&buf[PRI_PATH..]);
        out.push(Region {
            start,
            end: start + size,
            perms: format!("{}/{}", prot(rd_u32(&buf, PRI_PROTECTION)), prot(rd_u32(&buf, PRI_MAX_PROTECTION))),
            kind: format!("{} {}", share_mode(share), user_tag(tag)).trim().to_string(),
            resident: Some(rd_u32(&buf, PRI_PAGES_RESIDENT) as u64 * page_size),
            path,
        });
        addr = start + size;
    }
    out
}

fn prot(p: u32) -> String {
    let b = |bit, c| if p & bit != 0 { c } else { '-' };
    [b(1, 'r'), b(2, 'w'), b(4, 'x')].iter().collect()
}

fn share_mode(m: u32) -> &'static str {
    match m {
        1 => "COW",
        2 => "PRV",
        3 => "NUL",
        4 => "SHM",
        5 => "SHR",
        6 => "P/A",
        7 => "S/A",
        8 => "LPG",
        _ => "?",
    }
}

fn user_tag(t: u32) -> &'static str {
    match t {
        1 => "MALLOC",
        2 => "MALLOC_SMALL",
        3 => "MALLOC_LARGE",
        4 => "MALLOC_HUGE",
        5 => "SBRK",
        7 => "MALLOC_TINY",
        11 => "MALLOC_NANO",
        12 => "MALLOC_MEDIUM",
        20 => "MACH_MSG",
        21 => "IOKIT",
        30 => "STACK",
        31 => "GUARD",
        32 => "SHARED_PMAP",
        33 => "DYLIB",
        35 => "UNSHARED_PMAP",
        60 => "DYLD",
        61 => "DYLD_MALLOC",
        _ => "",
    }
}

fn threads(pid: Pid) -> Vec<ThreadInfo> {
    let mut ids = vec![0u64; 4096];
    let n =
        unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTTHREADS, 0, ids.as_mut_ptr().cast(), (ids.len() * 8) as c_int) };
    if n <= 0 {
        return Vec::new();
    }
    ids.truncate(n as usize / 8);
    ids.iter()
        .filter_map(|&id| {
            let t = pidinfo::<libc::proc_threadinfo>(pid, libc::PROC_PIDTHREADINFO, id)?;
            Some(ThreadInfo {
                tid: id,
                name: cstr(&t.pth_name),
                state: match t.pth_run_state {
                    1 => "running",
                    2 => "stopped",
                    3 => "waiting",
                    4 => "uninterruptible",
                    5 => "halted",
                    _ => "?",
                }
                .into(),
                cpu_time_ns: Some(t.pth_user_time + t.pth_system_time),
                priority: Some(t.pth_curpri),
            })
        })
        .collect()
}

fn state(bsd: &libc::proc_bsdinfo, task: Option<&libc::proc_taskinfo>) -> ProcState {
    match bsd.pbi_status {
        libc::SZOMB => ProcState::Zombie,
        libc::SSTOP => ProcState::Stopped,
        libc::SIDL => ProcState::Idle,
        // p_stat is SRUN for almost every live process; whether it is actually
        // on a CPU right now is per-thread, so look at the running-thread count.
        libc::SRUN | libc::SSLEEP => match task {
            Some(t) if t.pti_numrunning > 0 => ProcState::Running,
            Some(_) => ProcState::Sleeping,
            None => ProcState::Unknown,
        },
        _ => ProcState::Unknown,
    }
}

fn p_stat_name(s: u32) -> &'static str {
    match s {
        libc::SIDL => "SIDL",
        libc::SRUN => "SRUN",
        libc::SSLEEP => "SSLEEP",
        libc::SSTOP => "SSTOP",
        libc::SZOMB => "SZOMB",
        _ => "?",
    }
}

fn policy_name(p: i32) -> String {
    match p {
        1 => "POLICY_TIMESHARE (1)".into(),
        2 => "POLICY_RR (2)".into(),
        4 => "POLICY_FIFO (4)".into(),
        p => p.to_string(),
    }
}

fn bsd_name(bsd: &libc::proc_bsdinfo) -> String {
    let name = cstr(&bsd.pbi_name);
    if name.is_empty() { cstr(&bsd.pbi_comm) } else { name }
}

fn cstr(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn cstr_bytes(b: &[u8]) -> String {
    CStr::from_bytes_until_nul(b).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
}

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_ne_bytes(b[o..o + 2].try_into().unwrap())
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_ne_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_i32(b: &[u8], o: usize) -> i32 {
    i32::from_ne_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_ne_bytes(b[o..o + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_volume_is_its_apfs_container() {
        let v = volumes().expect("getfsstat");
        let root = v.list.iter().find(|v| v.mount == "/").expect("/ is listed");
        assert!(root.shown && root.size > 0 && root.used > 0, "{root:?}");
        eprintln!("{:#?}", v.list.iter().map(|v| (&v.mount, v.shown, v.percent(), &v.note)).collect::<Vec<_>>());
    }

    #[test]
    fn procargs_layout() {
        let mut b = 2i32.to_ne_bytes().to_vec();
        b.extend_from_slice(b"/bin/sleep\0\0\0\0sleep\x0060\0HOME=/Users/x\0TERM=xterm\0\0junk");
        let (exec, argv, env) = parse_procargs(&b);
        assert_eq!(exec, "/bin/sleep");
        assert_eq!(argv, ["sleep", "60"]);
        assert_eq!(env, ["HOME=/Users/x", "TERM=xterm"]);
    }

    #[test]
    fn sees_itself() {
        let mut c = MacCollector::new();
        let me = std::process::id() as Pid;
        let snap = c.snapshot();
        let p = snap.processes.iter().find(|p| p.pid == me).expect("own pid listed");
        assert!(p.rss > 0 && p.threads >= 1 && !p.restricted);
        assert!(snap.processes.iter().any(|p| p.pid == 1 && p.name == "launchd"), "root's launchd via kinfo_proc");
        let d = c.details(ProcId::local(me)).unwrap();
        assert!(d.files.iter().any(|f| f.fd == 0 || f.fd == 1 || f.fd == 2));
        assert!(!d.regions.is_empty());
        assert!(!d.threads.is_empty());
    }

    #[test]
    fn reads_network_and_disk_counters() {
        let sys = MacCollector::new().system();
        let net = sys.net.expect("NET_RT_IFLIST2");
        let disk = sys.disk.expect("IOBlockStorageDriver");
        assert!(net.read > 0 && disk.read > 0, "{net:?} {disk:?}");
        eprintln!("{net:?}\n{disk:?}\n{:?}\n{:?}\n{:?}\n{:?}", sys.gpu, sys.cpu, sys.battery, sys.thermal);
    }
}
