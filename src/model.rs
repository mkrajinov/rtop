//! Platform-neutral data model.
//!
//! Every value shown in the detail view is a [`Field`] that remembers *where*
//! the kernel exposed it (`source`) so the explain panel and the raw viewer can
//! show the reader exactly which file, struct member or syscall produced it.

pub type Pid = i32;

/// Which process a row is. A PID is only unique inside one PID namespace: two
/// containers of a pod each have their own PID 1. `space` numbers the
/// namespaces rtop reads from; it is 0 locally, on a node, and in a pod whose
/// containers share one namespace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcId {
    pub space: u16,
    pub pid: Pid,
}

impl ProcId {
    pub const fn local(pid: Pid) -> Self {
        ProcId { space: 0, pid }
    }
}

/// The PID as the kernel inside that namespace knows it; the container is
/// shown in its own column.
impl std::fmt::Display for ProcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.pid.fmt(f)
    }
}

/// Where a process lives when rtop reads a Kubernetes target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// `namespace/name`; only set on a node, where processes of many pods mix.
    pub pod: Option<std::sync::Arc<str>>,
    /// Container name, `host` for node processes outside any pod, or `pause`.
    pub container: std::sync::Arc<str>,
}

/// How a remote target is doing, for the header line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// The context came from the kubeconfig rather than from `--context`,
    /// so the header highlights it: it may not be the cluster you think.
    pub implicit_context: bool,
    /// Age of the oldest sample on screen, once it is older than two refreshes.
    pub stale: Option<std::time::Duration>,
    /// What rtop added to the cluster or could not read.
    pub notes: Vec<String>,
}

/// What rtop is attached to; `None` everywhere in local mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Pod {
        context: String,
        namespace: String,
        pod: String,
        node: Option<String>,
        container: Option<String>,
    },
    /// `detail`: allocatable capacity and pod count, for the header.
    Node {
        context: String,
        node: String,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcState {
    Running,
    Sleeping,
    DiskSleep,
    Stopped,
    Traced,
    Zombie,
    Dead,
    Idle,
    Unknown,
}

impl ProcState {
    /// The single-letter code `ps` and `top` use.
    pub fn letter(self) -> char {
        match self {
            ProcState::Running => 'R',
            ProcState::Sleeping => 'S',
            ProcState::DiskSleep => 'D',
            ProcState::Stopped => 'T',
            ProcState::Traced => 't',
            ProcState::Zombie => 'Z',
            ProcState::Dead => 'X',
            ProcState::Idle => 'I',
            ProcState::Unknown => '?',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProcState::Running => "running / runnable",
            ProcState::Sleeping => "interruptible sleep",
            ProcState::DiskSleep => "uninterruptible sleep",
            ProcState::Stopped => "stopped (job control)",
            ProcState::Traced => "stopped by tracer",
            ProcState::Zombie => "zombie (exited, not reaped)",
            ProcState::Dead => "dead",
            ProcState::Idle => "idle",
            ProcState::Unknown => "unknown (not visible to us)",
        }
    }

    /// Third field of `/proc/<pid>/stat`.
    pub fn from_linux(c: char) -> Self {
        match c {
            'R' => ProcState::Running,
            'S' => ProcState::Sleeping,
            'D' => ProcState::DiskSleep,
            'T' => ProcState::Stopped,
            't' => ProcState::Traced,
            'Z' => ProcState::Zombie,
            'X' | 'x' => ProcState::Dead,
            'I' => ProcState::Idle,
            _ => ProcState::Unknown,
        }
    }
}

/// One row in the process table. Cheap to collect for every process each tick.
#[derive(Clone, Debug)]
pub struct Process {
    /// PID namespace this row was read from; see [`ProcId`].
    pub space: u16,
    pub pid: Pid,
    pub ppid: Pid,
    pub pgid: Pid,
    pub uid: u32,
    pub user: String,
    pub name: String,
    pub cmdline: String,
    pub state: ProcState,
    pub nice: i32,
    pub priority: i32,
    /// user + system CPU time consumed so far, in nanoseconds.
    pub cpu_time_ns: u64,
    /// Filled in by [`crate::collector::cpu::CpuTracker`] from two samples.
    pub cpu_percent: f32,
    pub rss: u64,
    pub vsz: u64,
    pub threads: u32,
    pub fds: Option<u32>,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    /// The kernel refused to tell us some details (other user's process).
    pub restricted: bool,
    pub origin: Option<Origin>,
}

impl Process {
    pub fn id(&self) -> ProcId {
        ProcId { space: self.space, pid: self.pid }
    }

    pub fn parent_id(&self) -> ProcId {
        ProcId { space: self.space, pid: self.ppid }
    }
}

/// Cumulative CPU time counters for one core (unit is platform specific; only
/// ratios between two samples are meaningful).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTicks {
    pub busy: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    pub load: [f64; 3],
    pub mem_total: u64,
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub uptime_secs: u64,
    pub cores: Vec<CpuTicks>,
    /// Bytes received (`read`) and sent (`write`) by network interfaces.
    pub net: Option<IoCounters>,
    /// Bytes read and written by whole disks.
    pub disk: Option<IoCounters>,
    pub gpu: Option<GpuInfo>,
    pub cpu: CpuInfo,
    pub battery: Option<Battery>,
    pub thermal: Thermal,
    /// Mounted filesystems and their space; `None` where rtop can't read
    /// them (Kubernetes mode: `statvfs` is a syscall, not a file).
    pub volumes: Option<Volumes>,
    /// Bytes swapped in (`read`) and out (`write`) since boot: pages × page size.
    pub swap_io: Option<IoCounters>,
    pub pressure: Option<Pressure>,
    /// (runnable, total) scheduling entities from /proc/loadavg; `None`
    /// where the kernel doesn't say (macOS: the UI counts process states).
    pub run_queue: Option<(u32, u32)>,
    /// The interface traffic leaves by; `None` in Kubernetes mode.
    pub link: Option<NetLink>,
}

/// The main network interface: the default route's on Linux, the first
/// active `en*` with an IPv4 address on macOS.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetLink {
    pub name: String,
    pub ipv4: Option<String>,
    /// What the driver negotiated; `None` when it doesn't say (Wi-Fi on
    /// Linux, virtual interfaces).
    pub speed_mbps: Option<u64>,
    pub source: String,
}

/// How short of memory the kernel says it is right now.
#[derive(Clone, Debug, PartialEq)]
pub enum Pressure {
    /// macOS `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
    Level(u32),
    /// Linux PSI: share of the last 10 s in which some (or all) runnable
    /// tasks were stalled waiting for memory.
    Psi { some10: f32, full10: f32 },
}

/// Space on one filesystem, as `df` computes it from `statvfs(3)`/`statfs(2)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volume {
    pub mount: String,
    pub device: String,
    pub fstype: String,
    pub size: u64,
    /// Blocks in use: `(f_blocks - f_bfree) * frsize`.
    pub used: u64,
    /// What an unprivileged user may still write: `f_bavail * frsize`. The
    /// gap between `size - used` and this is the root reserve.
    pub free: u64,
    /// Drawn on the dashboard; the rest only appear in the `v` list.
    pub shown: bool,
    /// Why a volume is hidden or was not read.
    pub note: Option<String>,
}

impl Volume {
    /// Used share of what is usable, as `df`'s Capacity column.
    pub fn percent(&self) -> f32 {
        let usable = self.used + self.free;
        if usable == 0 { 0.0 } else { self.used as f32 / usable as f32 * 100.0 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volumes {
    /// Shown volumes first, fullest first; then the hidden ones.
    pub list: Vec<Volume>,
    pub source: String,
}

impl Volumes {
    pub fn new(mut list: Vec<Volume>, source: impl Into<String>) -> Self {
        list.sort_by(|a, b| {
            b.shown.cmp(&a.shown).then(b.percent().total_cmp(&a.percent())).then(a.mount.cmp(&b.mount))
        });
        Volumes { list, source: source.into() }
    }

    pub fn shown(&self) -> impl Iterator<Item = &Volume> {
        self.list.iter().filter(|v| v.shown)
    }
}

/// Temperatures in °C (the hottest sensor of each kind) and fans.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thermal {
    pub cpu_c: Option<f32>,
    pub gpu_c: Option<f32>,
    pub ssd_c: Option<f32>,
    pub fans: Vec<Fan>,
    pub source: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fan {
    pub rpm: f32,
    /// 0 when the hardware doesn't say.
    pub max: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CpuInfo {
    pub model: String,
    /// 'P' (performance) or 'E' (efficiency) per logical CPU; empty when all
    /// cores are the same kind.
    pub kinds: Vec<char>,
    /// Average current clock. macOS only reveals it to root (powermetrics).
    pub mhz: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    pub percent: f32,
    /// "charging", "on battery", "full", ...
    pub state: String,
    /// To full while charging, to empty otherwise.
    pub minutes: Option<u32>,
    /// (full charge capacity, design capacity, unit): their ratio is health.
    pub capacity: Option<(u64, u64, &'static str)>,
    pub cycles: Option<u32>,
    pub voltage_mv: Option<u32>,
    /// Negative while discharging.
    pub current_ma: Option<i32>,
    pub temp_c: Option<f32>,
    /// What the whole machine draws right now.
    pub system_watts: Option<f32>,
    pub adapter_watts: Option<u32>,
    pub source: String,
}

/// Two ever-growing byte counters. Like CPU time, only the difference
/// between two samples means anything: the UI turns it into bytes/second.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IoCounters {
    pub read: u64,
    pub write: u64,
    /// What was summed and where it came from, e.g. "en0 · NET_RT_IFLIST2".
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuInfo {
    pub name: String,
    /// Busy percentage. The driver already averages it over time, so unlike
    /// the byte counters it needs no second sample.
    pub busy: f32,
    pub mem_used: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub system: SystemInfo,
    pub processes: Vec<Process>,
    /// When the target took this sample, for collectors whose samples
    /// arrive late or twice (Kubernetes). CPU% divides by the time between
    /// two samples, so it must use this clock, not the time of arrival.
    /// `None`: the sample was taken just now.
    pub clock: Option<std::time::Instant>,
    /// Pod mode: what each container may use and uses (User Story 4).
    pub containers: Vec<ContainerUsage>,
}

/// One container of a pod: its cgroup's usage against its limits, and the
/// requests, limits and restarts from the pod object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContainerUsage {
    pub name: String,
    /// Cores used since the previous sample (`cpu.stat` usage_usec).
    pub cpu_cores: Option<f64>,
    /// `cpu.max` as cores; `None`: no limit.
    pub cpu_limit: Option<f64>,
    /// Share of scheduler periods since the previous sample in which the
    /// container hit its quota and waited.
    pub throttled_pct: Option<f64>,
    /// Since the container started: throttled periods and time waited.
    pub throttled_periods: Option<u64>,
    pub throttled_secs: Option<f64>,
    pub mem_current: Option<u64>,
    /// `memory.max`; `None`: no limit.
    pub mem_max: Option<u64>,
    pub mem_peak: Option<u64>,
    pub oom_kills: Option<u64>,
    /// From the pod spec, as written (`500m`, `256Mi`).
    pub cpu_request: Option<String>,
    pub mem_request: Option<String>,
    pub restarts: i32,
    pub last_termination: Option<String>,
    /// Where the cgroup files were read, or why they were not.
    pub source: String,
    /// The cgroup could not be read; the spec values are still shown.
    pub unreadable: bool,
}

/// A value plus its provenance.
#[derive(Clone, Debug)]
pub struct Field {
    /// Key into `assets/explain.toml`.
    pub key: &'static str,
    pub label: String,
    pub value: String,
    /// Exact origin, e.g. `/proc/42/status VmRSS` or `proc_taskinfo.pti_resident_size`.
    pub source: String,
    /// Text to search for in the raw sources to highlight where the value lives.
    pub raw_hint: Option<String>,
}

impl Field {
    pub fn new(key: &'static str, label: impl Into<String>, value: impl ToString, source: impl Into<String>) -> Self {
        Field { key, label: label.into(), value: value.to_string(), source: source.into(), raw_hint: None }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.raw_hint = Some(hint.into());
        self
    }
}

#[derive(Clone, Debug)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub perms: String,
    pub kind: String,
    pub resident: Option<u64>,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct OpenFile {
    pub fd: i32,
    pub kind: String,
    pub target: String,
}

#[derive(Clone, Debug)]
pub struct ThreadInfo {
    pub tid: u64,
    pub name: String,
    pub state: String,
    pub cpu_time_ns: Option<u64>,
    pub priority: Option<i32>,
}

/// Untouched kernel output, shown in the Raw tab.
#[derive(Clone, Debug)]
pub struct RawSource {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, Default)]
pub struct KernelView {
    pub sections: Vec<(String, Vec<Field>)>,
    /// Shown above the sections, e.g. to explain what a platform lacks.
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcessDetails {
    pub pid: Pid,
    pub name: String,
    pub overview: Vec<Field>,
    pub memory: Vec<Field>,
    pub regions: Vec<Region>,
    pub files: Vec<OpenFile>,
    pub threads: Vec<ThreadInfo>,
    pub env: Vec<(String, String)>,
    pub limits: Vec<Field>,
    pub kernel: KernelView,
    pub raw: Vec<RawSource>,
    /// Things we could not read and why (EPERM, EACCES, ...).
    pub notes: Vec<String>,
}
