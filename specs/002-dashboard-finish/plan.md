# Implementation Plan: Finish the dashboard

**Branch**: `main` | **Date**: 2026-11-05 | **Spec**: [spec.md](spec.md)

## Summary

Four independent additions to the dashboard, each a commit: volumes in the disk card (P1), swap
activity and memory pressure in the memory card (P2), run queue and a load graph in the system
card (P3), and network totals, address and link speed in the network card (P4). New values go
into `SystemInfo`, are filled by the macOS, Linux, Kubernetes and fixture collectors, and are
explained by entries that `L` cycles through (the Kubernetes lessons mechanism, extended to local
mode).

## Technical Context

Rust 2024, ratatui 0.30, `libc` (macOS FFI already used), `procfs-core` for parsing. No new
dependencies. Tests: unit tests for parsers and space math, `insta` snapshots from the fixture
collector, `make linux-test` in Docker.

## Constitution Check

| Principle | How this plan meets it |
|---|---|
| I Provenance | each new struct carries a `source` string shown in the card title or `v` list; README sources table gains volumes, swap, pressure, run queue, address, speed |
| II Teach first | explain entries `volumes`, `apfs_container`, `swap_activity`, `mem_pressure`, `run_queue`, `load_graph`, `net_link`; `L` reaches them outside Kubernetes too |
| III Both platforms | platform matrix in the spec; every gap has a message (k8s volumes, Linux without PSI, macOS run queue derived) |
| IV Deterministic tests | fixture collector gains all fields; parsers (`mountinfo`, `vmstat`, PSI, `loadavg` run queue, `/proc/net/route`) are pure and tested on macOS |
| V Cheap and safe | volumes cached 5 s; macOS `getfsstat(MNT_NOWAIT)`; Linux skips network/FUSE filesystems (listed as "not read") so `statvfs` can't hang |

No violations.

## Design

### Model (`src/model.rs`)

```rust
pub struct Volume { mount, device, fstype: String, size, used, free: u64, shown: bool, note: Option<String> }
pub struct Volumes { list: Vec<Volume>, source: String }          // SystemInfo.volumes: Option<Volumes>
pub enum Pressure { Level(u32), Psi { some10: f32, full10: f32 } } // SystemInfo.pressure: Option<Pressure>
pub struct SwapIo { ins, outs: u64 /* pages */, page_size: u64, source: String } // SystemInfo.swap_io
SystemInfo.run_queue: Option<(u32, u32)>                            // None on macOS → derived in UI
pub struct NetLink { name: String, ipv4: Option<String>, speed_mbps: Option<u64>, source: String } // SystemInfo.link
```

`shown` is false for volumes that are listed by `v` only (nobrowse, network). Rows on the card
are `shown` volumes sorted by used %, descending.

### Collection

- **macOS volumes**: `getfsstat(MNT_NOWAIT)`; group by APFS container (BSD name `diskN` of
  `/dev/diskNsM…`); container size = `f_blocks × f_bsize`, free = `f_bavail × f_bsize`, used =
  size − free; row named by the container's browsable mount (shortest), hidden if none is
  browsable. Non-APFS filesystems are one row each. Size 0 (autofs, devfs) skipped.
- **Linux volumes**: pure `parse_mountinfo` → (major:minor, mount, fstype, source); drop pseudo
  types; network types (`nfs*`, `cifs`, `smb3`, `ceph`, `9p`, every `fuse.*`) are listed with note "not read: network or FUSE filesystem"; dedupe by
  major:minor keeping the shortest mount; `statvfs` each. Pure `space(blocks, bfree, bavail,
  frsize)` shared by both.
- **Cache**: both collectors keep `(Instant, Volumes)` and re-read after 5 s.
- **Swap**: macOS `vm_statistics64.swapins/swapouts` (already fetched by `vm_stats()`), page
  size from `vm_stats` caller; Linux `/proc/vmstat` `pswpin`/`pswpout` × `sysconf(PAGESIZE)`.
- **Pressure**: macOS `sysctl kern.memorystatus_vm_pressure_level` (1 normal, 2 warn, 4
  critical); Linux `/proc/pressure/memory` → `some avg10`, `full avg10`.
- **Run queue**: `/proc/loadavg` field 4 `R/T` (R includes the reader itself, explained).
- **Link**: main interface = default route's. Linux: `/proc/net/route` destination
  `00000000` (pure parser); address via `getifaddrs`; speed `/sys/class/net/IF/speed` (`-1` or
  error → None). macOS: `getifaddrs` first up, non-loopback `en*` with IPv4 (the default
  route's interface on almost every Mac; full route lookup not worth the FFI), speed from
  `if_data64.ifi_baudrate` in the NET_RT_IFLIST2 walk.
- **Kubernetes**: `reader.sh` `_snap` adds `/proc/vmstat` and `/proc/pressure/memory`;
  `collector/kube.rs` parses them with the same functions; volumes and link stay `None`.

### App (`src/app.rs`)

- `swap_rate: RateTracker` → `swap_bps: [f32; 2]` (pages × page size as bytes/s).
- `load_hist: VecDeque<f32>` of `load1 / cores × 100`.
- `show_volumes: bool`, `v` toggles it outside labs (labs keep `v` = reveal); Esc closes.
- `L` in local mode cycles `DASH_LESSONS`; in Kubernetes mode `KUBE_LESSONS` then those.

### UI (`src/ui/header.rs`, `src/ui/mod.rs`)

- Disk card: inner width ≥ 36 → left half read/write stacked (head, 1-row graph, head, 1-row
  graph, faint `max` row), right half volume rows `mount  ■■■□□ 71%` and `N more · v`. Narrower →
  unchanged graphs, title hint `v volumes`.
- `v` overlay: table of every volume (mount, type, device, size, used, free, %, note), source
  line at the bottom.
- Memory card: title `pressure 3%` / `pressure warn` coloured by level; swap row label
  `swap ↓0 ↑48K/s`.
- System card: row 1 `load 2.41 1.98 1.70  runq 3/1210`; rows 2–3 `draw_graph` of
  `load_hist`; row 4 `tasks N ● z zombie ● d in D`; row 5 fans.
- Network card: right title `en0 192.168.1.20 · 1 Gb/s · <source>`; head `↓ in 2.1M/s · 14G`.

## Project Structure

Touches `src/model.rs`, `src/collector/{macos,linux,linux_parse,kube,fixture}.rs`,
`src/kube/reader.sh`, `src/app.rs`, `src/ui/{header,mod}.rs`, `assets/explain.toml`,
`tests/fixtures/linux/` (`mountinfo`, `vmstat`, `pressure_memory`, `route`), snapshots,
`README.md`, `ROADMAP.md`.
