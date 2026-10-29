//! Platform-neutral data model.
//!
//! Every value shown in the detail view is a [`Field`] that remembers *where*
//! the kernel exposed it (`source`) so the explain panel and the raw viewer can
//! show the reader exactly which file, struct member or syscall produced it.

pub type Pid = i32;

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
