//! Guided labs: rtop starts a misbehaving process tree and asks questions
//! about what you see.
//!
//! The TUI is multi-threaded, and forking a multi-threaded program is only
//! safe if the child immediately execs. So rtop re-executes itself as
//! `rtop __lab-worker <name>` (a fresh, single-threaded process) and the
//! worker does the fork/vfork/unshare tricks.

pub mod worker;

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use crate::model::{Pid, Process};

pub struct Step {
    pub task: &'static str,
    pub answer: &'static str,
}

pub struct Lab {
    pub name: &'static str,
    pub title: &'static str,
    pub linux_only: bool,
    pub intro: &'static str,
    pub steps: &'static [Step],
}

pub fn find(name: &str) -> Option<&'static Lab> {
    LABS.iter().find(|l| l.name == name)
}

pub fn available(lab: &Lab) -> bool {
    !lab.linux_only || cfg!(target_os = "linux")
}

/// A running lab: the worker's process group plus checklist progress.
pub struct LabSession {
    pub lab: &'static Lab,
    pub pgid: Pid,
    pub step: usize,
    pub revealed: bool,
    pub exit_note: Option<String>,
    child: Option<Child>,
}

impl LabSession {
    pub fn start(lab: &'static Lab) -> anyhow::Result<Self> {
        let child = Command::new(std::env::current_exe()?)
            .args(["__lab-worker", lab.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // setpgid(0, 0) in the child: the worker leads a new process group,
            // so one killpg() reaches it and everything it forks.
            .process_group(0)
            .spawn()?;
        Ok(LabSession { lab, pgid: child.id() as Pid, step: 0, revealed: false, exit_note: None, child: Some(child) })
    }

    /// Reap the worker if it exited (rtop is its parent; without this it
    /// would linger as rtop's own zombie).
    pub fn poll(&mut self) {
        let Some(child) = self.child.as_mut() else { return };
        if let Ok(Some(status)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                // A grandchild could still hold the pipe open; never block the UI on it.
                use std::os::fd::AsRawFd;
                unsafe { libc::fcntl(e.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
                let _ = e.read_to_string(&mut err);
            }
            self.exit_note = Some(match (status.code(), err.trim()) {
                (Some(0), "") => "worker exited normally and rtop reaped it with waitpid()".into(),
                (_, "") => format!("worker exited ({status}); rtop reaped it"),
                (_, e) => format!("worker failed: {e}"),
            });
            self.child = None;
        }
    }

    pub fn in_scope(&self, p: &Process) -> bool {
        p.pgid == self.pgid
    }

    pub fn current(&self) -> &'static Step {
        &self.lab.steps[self.step.min(self.lab.steps.len() - 1)]
    }

    pub fn next(&mut self) {
        if self.step + 1 < self.lab.steps.len() {
            self.step += 1;
            self.revealed = false;
        }
    }

    pub fn prev(&mut self) {
        if self.step > 0 {
            self.step -= 1;
            self.revealed = false;
        }
    }
}

impl Drop for LabSession {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(self.pgid), nix::sys::signal::Signal::SIGKILL);
        if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}

pub const LABS: &[Lab] = &[
    Lab {
        name: "zombie",
        title: "Zombies: exited but not reaped",
        linux_only: false,
        intro: "The worker forked 3 children that exit immediately. It never calls wait(), so the kernel has to keep each child's exit status around. A child in that leftover state is a zombie.",
        steps: &[
            Step {
                task: "Find the three zombies under the worker. What are their state letter, RSS and command line?",
                answer: "State Z, RSS 0 and an empty command line. A zombie has already freed its memory, fds and argv. All that remains is its process-table entry, which holds the exit status and resource usage until the parent collects them with wait().",
            },
            Step {
                task: "Select a zombie, press k and send SIGKILL. What happens?",
                answer: "Nothing. A zombie isn't running, so there is nothing left to kill, and signals sent to it are discarded. Only the parent reaping it removes it.",
            },
            Step {
                task: "Open the parent (Enter). What is it doing? On Linux, check wchan. Why isn't it reaping?",
                answer: "It is in S, sleeping in nanosleep, not in wait4/do_wait. The kernel sent it SIGCHLD each time a child exited, but it ignores the signal and never calls wait(). This is the classic bug.",
            },
            Step {
                task: "Kill the parent (k → SIGTERM) and watch the zombies.",
                answer: "They disappear. Orphans are reparented to PID 1 (launchd, init or the container's init) or to a subreaper, and those reap their children right away. rtop then reaps the worker itself, because rtop is its parent. The lesson: a zombie is a symptom, so fix the parent.",
            },
        ],
    },
    Lab {
        name: "orphan",
        title: "Orphans and reparenting",
        linux_only: false,
        intro: "The worker forks a child, then exits after 15 seconds. The child keeps running.",
        steps: &[
            Step {
                task: "Note the child's PPID now (Enter → Overview → Parent PID).",
                answer: "It is the worker's PID, and tree mode shows the child nested under the worker.",
            },
            Step {
                task: "Wait about 15 s for the worker to exit. What is the child's PPID now?",
                answer: "It becomes 1 (launchd on macOS; init, systemd or the container's PID 1 on Linux), or a subreaper such as `systemd --user`. The kernel reparents orphans so that someone can still wait() for them. The child is still in the lab's process group, so rtop keeps showing it.",
            },
            Step {
                task: "Why didn't the child die along with its parent?",
                answer: "Children are never killed just because their parent exited. Only a terminal hang-up (SIGHUP to the session) or Linux's prctl(PR_SET_PDEATHSIG) does that. Classic daemons rely on this: fork, let the parent exit, then call setsid().",
            },
        ],
    },
    Lab {
        name: "fdleak",
        title: "File-descriptor leak",
        linux_only: false,
        intro: "The worker lowers its soft RLIMIT_NOFILE to 256, then opens /dev/null every 100 ms and never closes it.",
        steps: &[
            Step {
                task: "Watch the FD column climb. Open the process (Enter) and look at the Files tab.",
                answer: "Each new fd gets the lowest unused number, and they all point at /dev/null (CHR). fds 0–2 are stdin, stdout and stderr.",
            },
            Step {
                task: "Open the Limits tab. What happens at 256?",
                answer: "On Linux the tab shows 'Max open files soft 256'. Once the process holds that many fds, open() fails with EMFILE ('Too many open files'). macOS can't read another process's limits, but the worker set its own with setrlimit().",
            },
            Step {
                task: "At what count does the FD column stop growing, and why?",
                answer: "At 256 (fds 0–255). RLIMIT_NOFILE is one more than the highest fd number allowed. The worker stops at its first EMFILE. A real server would start failing to accept() connections at this point.",
            },
            Step {
                task: "How would you catch a leak like this in production?",
                answer: "Graph the fd count over time (`ls /proc/PID/fd | wc -l`, or process_open_fds from node_exporter) and alert on its slope. Then check lsof or the Files tab to see what is leaking. Lots of sockets in CLOSE_WAIT means the app never closes connections the peer has already closed.",
            },
        ],
    },
    Lab {
        name: "cpuhog",
        title: "CPU contention and nice",
        linux_only: false,
        intro: "The worker starts one busy-loop process per CPU core at nice 0, and the same number again at nice 10. That gives twice as many runnable tasks as cores, so the scheduler has to choose. They stop by themselves after 2 minutes.",
        steps: &[
            Step {
                task: "Move the column focus to CPU% (← →) and press s to sort by it. Compare the nice 0 and nice 10 groups.",
                answer: "On Linux the nice 0 hogs get far more CPU: their weight is 1024 against 110, so they take roughly 90% of each contended core. macOS also favours them, but its QoS-driven scheduler narrows the gap.",
            },
            Step {
                task: "Why would they all show about 100% if there were free cores?",
                answer: "Nice only decides how a contended CPU is split. When there are idle cores, every runnable task gets a whole one.",
            },
            Step {
                task: "Open a nice 10 hog and look at its context switches.",
                answer: "A spinning task never blocks, so nearly all of its switches are involuntary (preemption). On Linux, a high nonvoluntary_ctxt_switches count is the fingerprint of CPU contention.",
            },
            Step {
                task: "Select a nice 0 hog and press + a few times. Then try -.",
                answer: "+ raises nice and shrinks its share, and needs no privileges. - fails with EACCES/EPERM: lowering nice requires root (or CAP_SYS_NICE on Linux). Otherwise any user could starve everyone else.",
            },
        ],
    },
    Lab {
        name: "memgrow",
        title: "Virtual vs resident memory",
        linux_only: false,
        intro: "The worker mmap()s 2 GiB of anonymous memory and waits 10 s. Then it writes to the memory 32 MiB at a time, up to 1 GiB.",
        steps: &[
            Step {
                task: "Compare VSZ and RSS during the first 10 seconds.",
                answer: "VSZ jumped by 2 GiB but RSS barely moved. mmap only reserves address space. No physical page exists until the program touches it.",
            },
            Step {
                task: "Watch RSS and the page-fault counter (Enter → Overview) while the worker writes.",
                answer: "RSS rises 32 MiB per step, and minor faults rise with it. Each first write to a page faults, and the kernel hands out a zero-filled physical page. This is demand paging.",
            },
            Step {
                task: "Open the Memory tab and find the 2 GiB region.",
                answer: "On Linux it is an anonymous rw-p mapping with no path. On macOS it is a PRV (private) region. Its address range never changes; only its resident part grows.",
            },
            Step {
                task: "What would happen in a container with memory.max = 512M?",
                answer: "The mmap would still succeed, because cgroups limit memory that is actually charged, not VSZ. Once touched pages push memory.current past memory.max, the kernel reclaims what it can and then OOM-kills a process inside that cgroup. In Kubernetes this shows up as 'OOMKilled'.",
            },
        ],
    },
    Lab {
        name: "threads",
        title: "Threads inside one process",
        linux_only: false,
        intro: "The worker starts 8 named threads. Four of them sleep and four do light CPU work.",
        steps: &[
            Step {
                task: "What does the THR column show for the worker, and why that number?",
                answer: "9: the 8 workers plus the main thread. All threads share one PID (the thread-group ID), and each has its own TID.",
            },
            Step {
                task: "Open it and go to the Threads tab. Compare the threads' states and CPU times.",
                answer: "The busy threads accumulate CPU time; the sleepers stay in S/waiting. The kernel schedules threads, not processes, and the process's CPU% is the sum over its threads.",
            },
            Step {
                task: "Where do the thread names come from?",
                answer: "std::thread::Builder::name calls pthread_setname_np. On Linux that ends up in /proc/PID/task/TID/comm (15 characters max). On macOS it ends up in proc_threadinfo.pth_name.",
            },
        ],
    },
    Lab {
        name: "dstate",
        title: "Uninterruptible sleep (D state)",
        linux_only: true,
        intro: "The worker vforks: clone(CLONE_VM|CLONE_VFORK), the same thing vfork() and posix_spawn() do. The parent is frozen inside the kernel until the child calls exec() or _exit(). The child just sleeps for 60 s, so the parent stays blocked the whole time.",
        steps: &[
            Step {
                task: "Find the parent. What state and wchan does it show?",
                answer: "It shows D and is waiting in wait_for_vfork_done. That is not an ordinary signal-interruptible wait.",
            },
            Step {
                task: "Watch the 1-minute load average in the header for a minute.",
                answer: "It climbs toward +1 even though no CPU is being used. Linux counts both R and D tasks in the load average. This is why NFS hangs or failing disks produce huge loads on idle CPUs.",
            },
            Step {
                task: "Send SIGTERM to the D-state parent. Then try SIGKILL.",
                answer: "SIGTERM does nothing while the task is in D, because signals are only acted on when it leaves the kernel wait. vfork's wait happens to be TASK_KILLABLE, so SIGKILL does get through. Many real D states, such as a hard NFS mount, are not killable at all.",
            },
        ],
    },
    Lab {
        name: "namespace",
        title: "Build half a container",
        linux_only: true,
        intro: "The worker unshares user, PID, UTS, IPC and network namespaces. It then forks a child, which becomes PID 1 of the new PID namespace and sets its hostname to 'rtop-lab'.",
        steps: &[
            Step {
                task: "Open the child and go to Kernel → Namespaces. Which namespaces differ from rtop's?",
                answer: "user, pid, uts, ipc and net differ. mnt, cgroup and time are shared with rtop. Two processes share a namespace exactly when the inode numbers match.",
            },
            Step {
                task: "On the Overview tab, what does 'PID in each PID namespace' show?",
                answer: "Something like '1234 → 1'. It is one task with two PIDs: 1234 on the host and 1 inside, just like a container's entrypoint.",
            },
            Step {
                task: "Look at Kernel → Privileges. Which capabilities does the child have?",
                answer: "All of them in CapEff, but only inside its own user namespace. Outside it, the process is still your unprivileged UID. This is how rootless containers work.",
            },
            Step {
                task: "What is missing for this to be a real container?",
                answer: "A mount namespace with its own root filesystem (pivot_root), a cgroup for resource limits, a seccomp filter, a reduced capability bounding set, and a veth pair to connect the empty network namespace. runc sets up all of these.",
            },
        ],
    },
];
