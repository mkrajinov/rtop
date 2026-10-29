# rtop roadmap

rtop exists to teach how the kernel manages processes, so features are ranked by how much they
teach, not only by how useful they are as a monitor. Every new number still needs:

- an `assets/explain.toml` entry;
- its provenance (the file or syscall it came from);
- a fixture test if it is parsed on Linux.

Each feature goes through Spec Kit (see `.specify/memory/constitution.md`):
`/speckit-specify <feature>`, then `/speckit-plan`, `/speckit-tasks` and `/speckit-implement`.
Specs are written to `specs/`.

Size: **S** is an evening, **M** a few days, **L** a week or more.

## Done

- [x] Process table: sort, filter, tree, follow, kill/signal picker, renice, pause
- [x] Detail tabs: Overview, Memory, Files, Threads, Env, Limits, Kernel, Raw
- [x] Explain panel and raw source viewer, with provenance for every field
- [x] Eight labs: zombie, orphan, fdleak, cpuhog, memgrow, threads, dstate, namespace
- [x] Dashboard: cpu (model, P/E cores, GHz), memory, system, battery, temperatures, fans, network, disk, gpu
- [x] Tailwind themes and accents, saved to `~/.config/rtop/config.toml`
- [x] Idle CPU about 1%, including under a key-repeat flood
- [x] Docker dev container (`make linux-up`, `make linux-exec`) with `rtop` on PATH

## Next: Kubernetes targets (spec: `specs/001-k8s-targets`)

- [x] `rtop k8s`: pick a namespace and pod in the current (or `--context`) context; Esc goes back to the list
- [x] `rtop k8s --pod ns/name`: processes of all containers in a pod, with a CONTAINER column; exec first, ephemeral debug container for distroless images
- [x] `rtop k8s --node name`: every process on a node, labelled with its pod and container or `host`, through a short-lived, read-only helper pod
- [ ] Nodes in the picker, and a container-aware dashboard (limits, throttling, OOM kills)

## Phase 1: finish the dashboard (small, visible wins)

| | Feature | Teaches | Linux source | macOS source | Size |
|---|---|---|---|---|---|
| [ ] | Disk space per volume (used/free bars) | mounts vs block devices; why `df` and `du` disagree | `/proc/self/mountinfo` + `statvfs` | `getfsstat` | S |
| [ ] | Network totals since boot, IP addresses, link speed | interface counters, addresses vs links | `/proc/net/dev`, `getifaddrs`, `/sys/class/net/*/speed` | `getifaddrs`, `NET_RT_IFLIST2` | S |
| [ ] | Swap and memory pressure in the memory card | reclaim, compressed memory, swap-in vs swap-out | `/proc/vmstat` (`pswpin`, `pswpout`), `/proc/pressure/memory` | `vm.memory_pressure`, `vm_stat` compressor pages | S |
| [ ] | Load average graph next to run-queue length | load ≠ CPU%; D-state tasks count toward load on Linux | `/proc/loadavg` (`running/total`) | `vm.loadavg` | S |
| [ ] | NVIDIA GPU | vendor APIs vs sysfs | NVML (`libnvidia-ml.so`, loaded at runtime) | n/a | M |

## Phase 2: per-process depth

| | Feature | Teaches | Linux source | macOS source | Size |
|---|---|---|---|---|---|
| [ ] | Per-process I/O columns (read/s, write/s) | `rchar` vs `read_bytes`; page cache hits never reach the disk | `/proc/PID/io` | `proc_pid_rusage` (`ri_diskio_*`) | S |
| [ ] | Per-process network sockets tab with TCP states | the socket → inode → fd chain | `/proc/net/tcp*`, `/proc/PID/fd` | `proc_pidfdinfo` (`PROC_PIDFDSOCKETINFO`) | M |
| [ ] | Container view: group processes by container | a container is just namespaces plus a cgroup | `/proc/PID/cgroup`, `ns/*` inodes, Docker cgroup path | n/a (explain why) | M |
| [ ] | Signal disposition view (blocked, ignored, caught, pending) | why `SIGKILL` always works and `SIGTERM` may not | `SigBlk/SigIgn/SigCgt/SigPnd` in `status` | `proc_pidinfo` (`pbsi_*`) where available | S |
| [ ] | Memory map heat: RSS per mapping, sorted | shared libraries, heap, stack, anonymous vs file-backed | `/proc/PID/smaps` | `proc_regioninfo` | M |
| [ ] | File locks and who holds them | advisory vs mandatory locks; flock vs fcntl | `/proc/locks` | n/a | S |

## Phase 3: system-wide kernel views

| | Feature | Teaches | Source | Size |
|---|---|---|---|---|
| [ ] | PSI view: cpu, memory and io pressure with `some`/`full` graphs, per cgroup | stall time vs utilisation | `/proc/pressure/*`, `cgroup/*.pressure` | M |
| [ ] | cgroup tree with limits and throttling | `cpu.max` throttling, `memory.max` and the OOM killer | `/sys/fs/cgroup` (`cpu.stat` `nr_throttled`, `memory.events`) | M |
| [ ] | Interrupts and softirqs per CPU | where CPU time goes outside processes | `/proc/interrupts`, `/proc/softirqs` | S |
| [ ] | Scheduler view: per-CPU run queue, context switches/s, migrations | CFS/EEVDF, runnable vs running | `/proc/schedstat`, `/proc/stat` (`ctxt`, `procs_running`) | M |
| [ ] | Page cache and dirty pages | writeback; why free memory looks low | `/proc/meminfo` (`Cached`, `Dirty`, `Writeback`) | S |

## Phase 4: new labs

| | Lab | What you see | Platform | Size |
|---|---|---|---|---|
| [ ] | `oom` | a cgroup with `memory.max`; `memory.events` counts up; the OOM killer picks a victim by `oom_score` | Linux | M |
| [ ] | `throttle` | `cpu.max` set to 20%; the process runs, then waits; `nr_throttled` climbs | Linux | S |
| [ ] | `cow` | fork, then write: RSS and PSS split as pages are copied | both | S |
| [ ] | `signals` | handler installed, signal blocked, then pending; `SigCgt`/`SigPnd` bits flip | both | S |
| [ ] | `pagecache` | read a large file twice; the second read never touches the disk (`read_bytes` stays 0) | Linux | S |
| [ ] | `timewait` | close sockets and watch them sit in TIME_WAIT | both | S |
| [ ] | `pipe` | a writer blocks when the 64 KiB pipe buffer is full; wchan shows `pipe_write` | Linux | S |

## Phase 5: tracing with eBPF (Linux)

These events can't be seen by polling `/proc`. They need `aya` and a privileged container.

| | Feature | Teaches | Size |
|---|---|---|---|
| [ ] | Exec and exit stream (short-lived processes `/proc` polling misses) | `sched_process_exec`/`exit` tracepoints | M |
| [ ] | Syscall counts per process, top-N | the user/kernel boundary; `raw_syscalls:sys_enter` | M |
| [ ] | Off-CPU time: why a process is waiting | scheduler tracepoints, stack traces | L |
| [ ] | TCP connect/accept/retransmit stream | the TCP state machine live | M |
| [ ] | File open stream per process | `openat` and how path lookup works | S |

## Phase 6: polish and infrastructure

- [x] `git init`, first commit, pushed to `github.com/mkrajinov/rtop`
- [x] Spec Kit set up with the project constitution
- [ ] CI: `make test lint` on macOS and Linux (GitHub Actions or GitLab CI)
- [ ] Install clippy in the Docker image so `make lint` also runs on Linux
- [ ] Mouse support: click to select, scroll, and click a column header to sort
- [ ] Record and replay: save a session of samples to a file and step through it later (good for labs)
- [ ] `rtop inspect PID --json` for scripting
- [ ] Configurable dashboard: choose and order the cards in `config.toml`
- [ ] Release binaries and a Homebrew tap

## Suggested order

1. Phase 1 (disk space, net totals, swap/pressure). Small, and it completes the dashboard.
2. Per-process I/O and the `pagecache` lab. They teach the same lesson from two sides.
3. Container view, cgroup tree and the `oom` and `throttle` labs. These fit an SRE background and the Docker setup.
4. PSI view.
5. eBPF, once the polling views are done, because tracing is easiest to understand next to what polling misses.
