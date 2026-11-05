//! A deterministic collector for UI tests: never touches the running system.

use anyhow::bail;

use super::Collector;
use crate::model::*;

pub fn process(pid: Pid, ppid: Pid, name: &str) -> Process {
    Process {
        space: 0,
        pid,
        ppid,
        pgid: pid,
        uid: 501,
        user: "learner".into(),
        name: name.into(),
        cmdline: name.into(),
        state: ProcState::Sleeping,
        nice: 0,
        priority: 20,
        cpu_time_ns: 0,
        cpu_percent: 0.0,
        rss: 4 << 20,
        vsz: 64 << 20,
        threads: 1,
        fds: Some(4),
        start_time: 1_700_000_000,
        restricted: false,
        origin: None,
    }
}

pub struct FixtureCollector {
    pub snapshot: Snapshot,
    /// Pretend to read a Kubernetes target.
    pub target: Option<Target>,
    pub link: Option<Link>,
}

impl Default for FixtureCollector {
    fn default() -> Self {
        let mut init = process(1, 0, "init");
        init.uid = 0;
        init.user = "root".into();
        init.cmdline = "/sbin/init".into();

        let mut shell = process(100, 1, "bash");
        shell.cmdline = "-bash".into();
        shell.cpu_percent = 1.5;

        let mut hog = process(200, 100, "cpuhog");
        hog.cmdline = "./cpuhog --threads 2".into();
        hog.state = ProcState::Running;
        hog.cpu_percent = 199.0;
        hog.cpu_time_ns = 3_723_000_000_000;
        hog.threads = 3;
        hog.rss = 512 << 20;
        hog.vsz = 2 << 30;
        hog.nice = 10;
        hog.priority = 30;

        let mut zombie = process(201, 100, "defunct");
        zombie.cmdline = String::new();
        zombie.state = ProcState::Zombie;
        zombie.rss = 0;
        zombie.vsz = 0;
        zombie.fds = None;

        FixtureCollector {
            snapshot: Snapshot {
                system: SystemInfo {
                    load: [1.25, 0.75, 0.5],
                    mem_total: 16 << 30,
                    mem_used: 6 << 30,
                    swap_total: 2 << 30,
                    swap_used: 0,
                    uptime_secs: 93_784,
                    cores: vec![CpuTicks::default(); 4],
                    net: Some(IoCounters { read: 1 << 30, write: 1 << 28, source: "eth0 · /proc/net/dev".into() }),
                    disk: Some(IoCounters { read: 1 << 34, write: 1 << 33, source: "vda · /proc/diskstats".into() }),
                    gpu: None,
                    cpu: CpuInfo { model: "Fixture CPU".into(), kinds: vec!['E', 'E', 'P', 'P'], mhz: None },
                    battery: None,
                    thermal: Thermal {
                        cpu_c: Some(62.0),
                        fans: vec![Fan { rpm: 2400.0, max: 6000.0 }],
                        ..Default::default()
                    },
                    volumes: Some(Volumes::new(
                        vec![
                            Volume {
                                mount: "/".into(),
                                device: "/dev/vda1".into(),
                                fstype: "ext4".into(),
                                size: 64 << 30,
                                used: 45 << 30,
                                free: 16 << 30,
                                shown: true,
                                note: None,
                            },
                            Volume {
                                mount: "/var/lib/docker".into(),
                                device: "/dev/vdb".into(),
                                fstype: "xfs".into(),
                                size: 256 << 30,
                                used: 230 << 30,
                                free: 26 << 30,
                                shown: true,
                                note: None,
                            },
                            Volume {
                                mount: "/boot".into(),
                                device: "/dev/vda2".into(),
                                fstype: "ext4".into(),
                                size: 1 << 30,
                                used: 200 << 20,
                                free: 760 << 20,
                                shown: true,
                                note: None,
                            },
                            Volume {
                                mount: "/srv/nfs".into(),
                                device: "fs1:/export".into(),
                                fstype: "nfs4".into(),
                                note: Some("not read: a network or FUSE filesystem can block statvfs".into()),
                                ..Default::default()
                            },
                        ],
                        "/proc/self/mountinfo + statvfs",
                    )),
                    swap_io: Some(IoCounters { read: 4 << 12, write: 9 << 12, source: "/proc/vmstat".into() }),
                    pressure: Some(Pressure::Psi { some10: 3.4, full10: 0.8 }),
                    run_queue: Some((2, 312)),
                },
                processes: vec![init, shell, hog, zombie],
                clock: None,
                containers: Vec::new(),
            },
            target: None,
            link: None,
        }
    }
}

impl Collector for FixtureCollector {
    fn platform(&self) -> &'static str {
        "fixture"
    }

    fn snapshot(&mut self) -> Snapshot {
        self.snapshot.clone()
    }

    fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    fn link(&self) -> Option<Link> {
        self.link.clone()
    }

    fn details(&mut self, id: ProcId) -> anyhow::Result<ProcessDetails> {
        let pid = id.pid;
        let Some(p) = self.snapshot.processes.iter().find(|p| p.id() == id) else {
            bail!("no such process: {pid}");
        };
        Ok(ProcessDetails {
            pid,
            name: p.name.clone(),
            overview: vec![
                Field::new("pid", "PID", pid, "fixture").hint("pid"),
                Field::new("ppid", "Parent PID", p.ppid, "fixture"),
                Field::new("state", "State", p.state.letter(), "fixture").hint("state"),
                Field::new("nice", "Nice", p.nice, "fixture"),
            ],
            memory: vec![Field::new("rss", "RSS", p.rss, "fixture"), Field::new("vsz", "VSZ", p.vsz, "fixture")],
            files: vec![OpenFile { fd: 0, kind: "CHR".into(), target: "/dev/null".into() }],
            raw: vec![RawSource { title: "fixture".into(), body: format!("pid {pid}\nstate {}\n", p.state.letter()) }],
            ..Default::default()
        })
    }
}
