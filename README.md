# rtop

An htop-style process explorer built for **learning how the kernel manages processes**.
Every number on screen can tell you what it means and where it came from:

- Linux reads it from a file under `/proc`.
- macOS reads it from a `proc_pidinfo(2)` / `sysctl(3)` struct.

## Run

```sh
make run                 # macOS or Linux, release build
cargo run -- lab         # list guided labs
cargo run -- lab zombie  # start one
cargo run -- inspect PID # print everything about one process, with sources
make linux-shell         # Linux dev container (sees the Docker VM's processes)
make linux-run           # rtop itself inside that container
make install             # link ~/.local/bin/rtop to the release build
```

Kubernetes (reads `/proc` inside pods over `pods/exec`; read-only unless `--allow-actions`):

```sh
rtop k8s                                # pick a namespace and pod in the current context
rtop k8s --context kind-rtop -n demo    # another context, starting in namespace demo
rtop k8s --pod demo/api-7c9f            # straight to one pod; Esc goes back to the list
```

## Keys

| Where | Key | Action |
|---|---|---|
| table | `← →` | focus a column (blue header) |
| | `s` | sort by the focused column (press again to reverse) |
| | `e` | explain panel for the focused column or field |
| | `[` `]` | scroll the explain panel |
| | `Enter` | inspect the process |
| | `/` | filter |
| | `t` | tree view |
| | `f` | show only this process and its descendants |
| | `k` | send a signal |
| | `+` `-` | renice |
| | `p` | pause refreshing |
| | `c` | show / hide executable paths |
| | `T` | theme picker (live preview, Enter saves) |
| | `?` | help |
| detail | `← →` / `1-8` | switch tab |
| | `↑ ↓` | select a field; the bottom line shows its exact source |
| | `r` | jump to the raw kernel data behind that field |
| lab | `n` / `b` | next / previous step |
| | `v` | reveal the answer |

Detail tabs: Overview, Memory (incl. the full memory map), Files (sockets resolved to
`addr:port STATE`), Threads, Env, Limits, Kernel (namespaces, cgroup v2 + PSI,
capabilities, seccomp, scheduler, signals), Raw.

## Dashboard

The cards above the process list: cpu, memory and system, plus network, disk and gpu when the
terminal has at least 38 rows. Under 22 rows the dashboard is hidden. Each card's title names the
data source.

| Card | macOS | Linux |
|---|---|---|
| cpu model, P/E cores | `machdep.cpu.brand_string`, `cluster-type` under IODeviceTree:/cpus | `/proc/cpuinfo`, `/sys/devices/cpu_core` + `cpu_atom` (Intel hybrid), cpufreq |
| battery (if present, under memory) | IOKit `AppleSmartBattery` (+ `PowerTelemetryData` for system watts) | `/sys/class/power_supply/BAT*` |
| temperatures (cpu, gpu, ssd) | HID event system, Apple sensors on usage page 0xff00 (`PMU tdie*`, `pACC MTR`, `GPU MTR`, `NAND`) | `/sys/class/hwmon/*/temp*_input` |
| fans (system card) | AppleSMC keys `FNum`, `F<n>Ac`, `F<n>Mx` | `/sys/class/hwmon/*/fan*_input` |
| network | `sysctl NET_RT_IFLIST2` (64-bit counters), `en*` interfaces only | `/proc/net/dev`, interfaces with a `/sys/class/net/*/device` |
| disk | IOKit `IOBlockStorageDriver` → `Statistics` | `/proc/diskstats`, whole disks from `/sys/block` |
| gpu | IOKit `IOAccelerator` → `PerformanceStatistics` | `/sys/class/drm/card*/device/gpu_busy_percent` (amdgpu; NVIDIA needs NVML) |

The kernel only counts bytes, so the card computes the rate as the difference between two samples,
the same way CPU% works. Each throughput graph is scaled to its own peak, shown as `max`.

## Theme

Every colour is an exact Tailwind CSS v4 palette value. You pick a **theme** and an **accent**.

- **Theme:** a Tailwind gray family for text and borders, plus Tailwind hues for status.
  - One per gray family: neutral, zinc, stone, slate, gray, taupe, mauve, olive, mist. These use emerald,
    amber, rose and violet for status.
  - Mixed themes: pastel (300 shades), vivid (500 shades), ocean, forest, sunset, dusk, frost.

  In every theme the status colours mean the same thing: green means fine, amber a warning, red bad,
  and purple a zombie. Values change colour in steps at 50% and 80%.
- **Accent:** any of Tailwind's 17 hues. Shade 400 colours titles and the sort column, shade 800 the
  focused column and active tab, and shade 950 the selected row.

To change it:

- Press `T` and choose with the arrow keys. The colours change as you browse. `Enter` saves the choice to
  `~/.config/rtop/config.toml` (or `$XDG_CONFIG_HOME/rtop/config.toml`). `Esc` restores the previous
  colours.
- For a single run, `rtop --theme nord --accent sky` overrides the saved choice.

```toml
# ~/.config/rtop/config.toml   (older files with `gray = ...` still work)
theme = "catppuccin"
accent = "violet"
```

## Labs

| Lab | What you see | Where |
|---|---|---|
| `zombie` | Z-state children: why `kill` can't remove them, and how reparenting reaps them | both |
| `orphan` | the PPID changes to 1 (or to a subreaper) when the parent exits | both |
| `fdleak` | the FD count climbs until EMFILE at RLIMIT_NOFILE | both |
| `cpuhog` | nice under contention; involuntary context switches | both |
| `memgrow` | VSZ vs RSS, demand paging, minor faults | both |
| `threads` | TIDs and thread names; per-thread CPU time | both |
| `dstate` | a task in D state (vfork wait); wchan; the load average rising with an idle CPU | Linux |
| `namespace` | user, pid, net, uts and ipc namespaces; NSpid; capabilities inside a user namespace | Linux |

Each lab runs as `rtop __lab-worker <name>`, a re-exec of rtop in its own process group. rtop kills the
whole group when you quit, and workers exit on their own after 10 minutes.

## Layout

- `src/collector/macos.rs` reads data via libproc, `sysctl` (falling back to `kinfo_proc` for other users'
  processes) and Mach host statistics.
- `src/collector/linux.rs` reads `/proc` and `/sys/fs/cgroup`.
- `src/collector/linux_parse.rs` parses that data. It does no I/O, so it is tested on every OS against
  `tests/fixtures/linux`.
- `assets/explain.toml` is the field catalog. A test fails if any field key used in the code is missing from it.
- `src/labs/` holds the lab definitions (`mod.rs`) and the processes they inspect (`worker.rs`).
- `tests/ui.rs` holds whole-screen `insta` snapshots, rendered from a fixed fixture collector.

## Test

```sh
make test lint      # macOS
make linux-test     # Linux, in Docker
```

Next features are listed in [ROADMAP.md](ROADMAP.md).
