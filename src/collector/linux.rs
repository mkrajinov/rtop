//! Linux backend: everything comes from text files under /proc and /sys.
//! Reading is done here; parsing lives in `linux_parse` so it can be tested
//! on any OS.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;

use super::linux_parse::{self as lp, LinuxEnv, ProcFiles};
use super::{Collector, UserCache};
use crate::model::*;

pub struct LinuxCollector {
    users: UserCache,
    env: LinuxEnv,
    cmdlines: HashMap<(Pid, u64), Vec<u8>>,
    cpu: CpuInfo,
}

impl LinuxCollector {
    pub fn new() -> Self {
        let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
        LinuxCollector {
            users: UserCache::default(),
            env: LinuxEnv {
                ticks: unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64,
                page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64,
                btime: lp::proc_stat(&stat).1,
            },
            cmdlines: HashMap::new(),
            cpu: cpu_info(),
        }
    }
}

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn link(path: &str) -> Option<String> {
    fs::read_link(path).ok().map(|p| p.to_string_lossy().into_owned())
}

fn pids() -> Vec<Pid> {
    fs::read_dir("/proc")
        .map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

fn cpu_info() -> CpuInfo {
    let (model, _) = lp::cpuinfo(&read("/proc/cpuinfo").unwrap_or_default());
    let model = model.unwrap_or_else(|| std::env::consts::ARCH.to_string());
    // Intel hybrid CPUs list their P-cores and E-cores as two PMU devices.
    let mut kinds = Vec::new();
    for (dev, kind) in [("cpu_core", 'P'), ("cpu_atom", 'E')] {
        for i in lp::cpulist(&read(&format!("/sys/devices/{dev}/cpus")).unwrap_or_default()) {
            if kinds.len() <= i {
                kinds.resize(i + 1, ' ');
            }
            kinds[i] = kind;
        }
    }
    if kinds.contains(&' ') {
        kinds.clear();
    }
    CpuInfo { model, kinds, mhz: None }
}

/// The average current clock: cpufreq if the kernel has a driver for it,
/// else the "cpu MHz" lines of /proc/cpuinfo (x86 only).
fn mhz() -> Option<f32> {
    let khz: Vec<f32> = fs::read_dir("/sys/devices/system/cpu")
        .ok()?
        .flatten()
        .filter_map(|e| read(&format!("{}/cpufreq/scaling_cur_freq", e.path().display()))?.trim().parse().ok())
        .collect();
    if khz.is_empty() {
        lp::cpuinfo(&read("/proc/cpuinfo")?).1
    } else {
        Some(khz.iter().sum::<f32>() / khz.len() as f32 / 1000.0)
    }
}

/// hwmon drivers publish temp*_input (millidegrees) and fan*_input (rpm).
/// The driver name says what a sensor measures.
fn thermal() -> Thermal {
    let mut t = Thermal { source: "/sys/class/hwmon".into(), ..Default::default() };
    let Ok(dirs) = fs::read_dir("/sys/class/hwmon") else { return t };
    for dir in dirs.flatten().map(|e| e.path()) {
        let name = read(&format!("{}/name", dir.display())).unwrap_or_default();
        let slot = match name.trim() {
            "coretemp" | "k10temp" | "zenpower" | "cpu_thermal" | "soc_thermal" => &mut t.cpu_c,
            "amdgpu" | "nouveau" | "radeon" => &mut t.gpu_c,
            "nvme" | "drivetemp" => &mut t.ssd_c,
            _ => &mut None,
        };
        let Ok(files) = fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let file = f.file_name().to_string_lossy().into_owned();
            let value = || read(&f.path().display().to_string())?.trim().parse::<f32>().ok();
            if file.starts_with("temp") && file.ends_with("_input") {
                if let Some(c) = value().map(|m| m / 1000.0) {
                    *slot = Some(slot.map_or(c, |old: f32| old.max(c)));
                }
            } else if file.starts_with("fan") && file.ends_with("_input") {
                let max = read(&f.path().display().to_string().replace("_input", "_max"))
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0.0);
                if let Some(rpm) = value() {
                    t.fans.push(Fan { rpm, max });
                }
            }
        }
    }
    t
}

fn battery() -> Option<Battery> {
    for e in fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let dir = e.path();
        if read(&format!("{}/type", dir.display())).is_none_or(|t| t.trim() != "Battery") {
            continue;
        }
        let attrs: HashMap<String, String> = fs::read_dir(&dir)
            .ok()?
            .flatten()
            .filter_map(|f| Some((f.file_name().into_string().ok()?, fs::read_to_string(f.path()).ok()?)))
            .collect();
        return lp::battery(&attrs, &dir.display().to_string());
    }
    None
}

/// Sums the rows that `keep` accepts. When some entry has a `device` link in
/// sysfs (real hardware), only those count: virtual interfaces and
/// device-mapper disks carry the same bytes a second time.
fn sum(rows: Vec<(String, u64, u64)>, sys: &str, keep: impl Fn(&str) -> bool, source: &str) -> Option<IoCounters> {
    let rows: Vec<_> = rows.into_iter().filter(|(n, ..)| keep(n)).collect();
    let physical = |n: &str| fs::metadata(format!("{sys}/{n}/device")).is_ok();
    let any_physical = rows.iter().any(|(n, ..)| physical(n));
    let mut c = IoCounters::default();
    let mut names = Vec::new();
    for (n, r, w) in &rows {
        if !any_physical || physical(n) {
            c.read += r;
            c.write += w;
            names.push(n.as_str());
        }
    }
    c.source = format!("{} · {source}", names.join(" "));
    Some(c)
}

fn net() -> Option<IoCounters> {
    let rows = lp::parse_net_dev(&read("/proc/net/dev")?);
    sum(rows, "/sys/class/net", |n| n != "lo", "/proc/net/dev")
}

fn disk() -> Option<IoCounters> {
    let rows = lp::parse_diskstats(&read("/proc/diskstats")?);
    // /sys/block lists whole disks only; partitions would double count.
    let whole = |n: &str| {
        fs::metadata(format!("/sys/block/{n}")).is_ok() && !["loop", "ram", "zram"].iter().any(|p| n.starts_with(p))
    };
    sum(rows, "/sys/block", whole, "/proc/diskstats")
}

/// amdgpu (and some other DRM drivers) publish gpu_busy_percent in sysfs.
/// NVIDIA does not: its numbers are only reachable through NVML.
fn gpu() -> Option<GpuInfo> {
    let cards = fs::read_dir("/sys/class/drm").ok()?;
    let mut best: Option<GpuInfo> = None;
    for e in cards.flatten() {
        let card = e.file_name().to_string_lossy().into_owned();
        // card0-DP-1 and friends are connectors, not GPUs.
        if !card.starts_with("card") || card.contains('-') {
            continue;
        }
        let dev = format!("/sys/class/drm/{card}/device");
        let Some(busy) = read(&format!("{dev}/gpu_busy_percent")).and_then(|s| s.trim().parse::<f32>().ok()) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| busy > b.busy) {
            let driver = link(&format!("{dev}/driver")).and_then(|d| Some(d.rsplit('/').next()?.to_string()));
            best = Some(GpuInfo {
                name: format!("{card} · {}", driver.unwrap_or_default()),
                busy,
                mem_used: read(&format!("{dev}/mem_info_vram_used")).and_then(|s| s.trim().parse().ok()),
            });
        }
    }
    best
}

fn ns_links(dir: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| {
                let e = e.ok()?;
                Some((e.file_name().to_string_lossy().into_owned(), link(e.path().to_str()?)?))
            })
            .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

impl Collector for LinuxCollector {
    fn platform(&self) -> &'static str {
        "Linux"
    }

    fn snapshot(&mut self) -> Snapshot {
        let system = lp::system(
            &read("/proc/stat").unwrap_or_default(),
            &read("/proc/meminfo").unwrap_or_default(),
            &read("/proc/loadavg").unwrap_or_default(),
            &read("/proc/uptime").unwrap_or_default(),
        );
        let cpu = CpuInfo { mhz: mhz(), ..self.cpu.clone() };
        let system =
            SystemInfo { net: net(), disk: disk(), gpu: gpu(), cpu, battery: battery(), thermal: thermal(), ..system };
        let mut processes = Vec::new();
        let mut live = std::collections::HashSet::new();
        for pid in pids() {
            let dir = format!("/proc/{pid}");
            let Some(stat) = read(&format!("{dir}/stat")) else { continue };
            // The owner of /proc/<pid> is the process's effective UID.
            let uid = fs::metadata(&dir).map(|m| m.uid()).unwrap_or(0);
            let fds = fs::read_dir(format!("{dir}/fd")).ok().map(|d| d.count() as u32);
            let start = lp::parse_stat(&stat).map(|s| s.starttime).unwrap_or(0);
            live.insert((pid, start));
            let cmd = self
                .cmdlines
                .entry((pid, start))
                .or_insert_with(|| fs::read(format!("{dir}/cmdline")).unwrap_or_default())
                .clone();
            let user = self.users.name(uid);
            if let Ok(p) = lp::process_row(&stat, uid, user, &cmd, fds, &self.env) {
                processes.push(p);
            }
        }
        self.cmdlines.retain(|k, _| live.contains(k));
        Snapshot { system, processes }
    }

    fn details(&mut self, pid: Pid) -> anyhow::Result<ProcessDetails> {
        let dir = format!("/proc/{pid}");
        let f = |name: &str| read(&format!("{dir}/{name}"));
        let stat = f("stat").ok_or_else(|| anyhow::anyhow!("{dir}/stat is gone: the process exited"))?;

        let fds: Option<Vec<(i32, String)>> = fs::read_dir(format!("{dir}/fd")).ok().map(|d| {
            let mut v: Vec<_> = d
                .filter_map(|e| {
                    let e = e.ok()?;
                    let fd = e.file_name().to_str()?.parse().ok()?;
                    Some((fd, link(e.path().to_str()?).unwrap_or_default()))
                })
                .collect();
            v.sort();
            v
        });

        // Socket fds only show an inode; the per-namespace tables say what it is.
        let mut sockets = HashMap::new();
        if fds.iter().flatten().any(|(_, t)| t.starts_with("socket:")) {
            for (file, proto, v6) in
                [("tcp", "TCP", false), ("tcp6", "TCP6", true), ("udp", "UDP", false), ("udp6", "UDP6", true)]
            {
                if let Some(t) = f(&format!("net/{file}")) {
                    sockets.extend(lp::parse_net(&t, proto, v6));
                }
            }
            if let Some(t) = f("net/unix") {
                sockets.extend(lp::parse_unix(&t));
            }
        }

        let cgroup = f("cgroup");
        let mut cgroup_files = Vec::new();
        if let Some(path) = cgroup.as_deref().and_then(|c| c.lines().find_map(|l| l.strip_prefix("0::"))) {
            for name in [
                "memory.current",
                "memory.max",
                "memory.peak",
                "memory.pressure",
                "cpu.max",
                "cpu.weight",
                "cpu.pressure",
                "pids.current",
                "pids.max",
                "io.pressure",
            ] {
                if let Some(v) = read(&format!("/sys/fs/cgroup{path}/{name}")) {
                    cgroup_files.push((name.to_string(), v));
                }
            }
        }

        let tasks = fs::read_dir(format!("{dir}/task"))
            .map(|d| {
                let mut v: Vec<(u64, String)> = d
                    .filter_map(|e| {
                        let e = e.ok()?;
                        let tid = e.file_name().to_str()?.parse().ok()?;
                        Some((tid, read(&format!("{}/stat", e.path().display()))?))
                    })
                    .collect();
                v.sort();
                v
            })
            .unwrap_or_default();

        let files = ProcFiles {
            pid,
            status: f("status").unwrap_or_default(),
            cmdline: fs::read(format!("{dir}/cmdline")).unwrap_or_default(),
            environ: fs::read(format!("{dir}/environ")).ok(),
            io: f("io"),
            limits: f("limits"),
            cgroup,
            cgroup_files,
            schedstat: f("schedstat"),
            smaps_rollup: f("smaps_rollup"),
            maps: f("maps"),
            wchan: f("wchan"),
            oom_score: f("oom_score"),
            oom_score_adj: f("oom_score_adj"),
            exe: link(&format!("{dir}/exe")),
            cwd: link(&format!("{dir}/cwd")),
            ns: ns_links(&format!("{dir}/ns")),
            self_ns: ns_links("/proc/self/ns"),
            fds,
            tasks,
            sockets,
            stat,
        };
        let users = &mut self.users;
        lp::details(&files, &self.env, &mut |uid| users.name(uid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_itself() {
        let mut c = LinuxCollector::new();
        let me = std::process::id() as Pid;
        let snap = c.snapshot();
        let p = snap.processes.iter().find(|p| p.pid == me).expect("own pid listed");
        assert!(p.rss > 0 && p.threads >= 1 && p.fds.is_some());
        assert!(!snap.system.cores.is_empty() && snap.system.mem_total > 0);
        let (net, disk) = (snap.system.net.as_ref().unwrap(), snap.system.disk.as_ref().unwrap());
        assert!(net.read > 0 && disk.read > 0, "{net:?} {disk:?}");
        eprintln!("{net:?}\n{disk:?}\n{:?}", snap.system.gpu);
        let d = c.details(me).unwrap();
        assert!(d.files.iter().any(|f| f.fd <= 2));
        assert!(!d.regions.is_empty() && !d.threads.is_empty() && !d.limits.is_empty());
        assert!(
            d.kernel
                .sections
                .iter()
                .any(|(n, f)| n == "Namespaces" && f.iter().all(|f| f.value.contains("same as rtop")))
        );
    }
}
