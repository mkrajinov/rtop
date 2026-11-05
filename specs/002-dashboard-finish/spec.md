# Feature Specification: Finish the dashboard

**Feature Branch**: `main` (no feature branch; commits per story)

**Created**: 2026-11-05

**Status**: Draft

**Input**: User description: "continue to implement next step" — ROADMAP Phase 1, the four S
items: disk space per volume, network totals with IP address and link speed, swap activity and
memory pressure, and a load average graph next to the run-queue length. NVIDIA GPU (M) is out of
scope.

## Clarifications

### Session 2026-11-05

- Q: Where do the volume bars go? → A: Split the disk card. Read/write graphs on the left; up to
  as many volumes as fit on the right, fullest first, then "N more · v to list". `v` opens a list
  of every volume.
- Q: Where do swap activity and pressure go? → A: Pressure as a chip in the memory card title;
  swap-in/out rates on the swap row. The memory sparkline stays.
- Q: Where does the load graph go? → A: In the system card, in place of the `1m 5m 15m` labels
  row. Row 1: the three load averages and `runq R/T`. Rows 2–3: a two-row graph of the 1-minute
  load, scaled to the number of cores. Tasks and fans share the last two rows.
- Q: Where do network totals, address and speed go? → A: The card title names the main
  interface, its IPv4 address and link speed; each half's head line adds the total since the
  counters started.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Disk space per volume (Priority: P1)

The disk card shows, next to its throughput graphs, the mounted volumes with a used/free bar and
percentage, fullest first. `v` lists every volume with size, used, free, filesystem type and
device. The explain panel says why `df` and `du` disagree (deleted-but-open files, reserved root
blocks, snapshots, shared APFS containers).

**Why this priority**: "is a disk filling up" is the most common question the dashboard can't
answer yet, and mounts vs block devices is a core lesson.

**Independent Test**: run rtop, compare each listed volume's size/used/free with `df -k`.

**Acceptance Scenarios**:

1. **Given** a Mac with the usual APFS volumes, **When** rtop starts, **Then** the disk card
   lists `/` once for its APFS container with the container's used % (Finder's figure, not
   `df`'s 43% for the sealed system volume), and free matches `df -k` Available.
2. **Given** Linux with bind mounts, tmpfs, overlay and proc, **When** rtop starts, **Then**
   pseudo filesystems are hidden and each block device appears once.
3. **Given** a hung network mount, **When** rtop refreshes, **Then** the UI keeps responding and
   that volume is marked as not answering.
4. **Given** Kubernetes mode, **When** the dashboard draws, **Then** the space half says why
   volumes aren't shown there.

---

### User Story 2 - Swap activity and memory pressure (Priority: P2)

The memory card title shows memory pressure; the swap row shows pages swapped in and out per
second. The explain entries teach reclaim, compression (macOS) and why swap *used* is not swap
*activity*.

**Why this priority**: swap used often sits high for days on a healthy machine; only the rates
and pressure say whether memory is short right now.

**Independent Test**: compare the rates with `vm_stat 1` (macOS) or `vmstat 1` (Linux) and the
pressure with `sysctl kern.memorystatus_vm_pressure_level` or `cat /proc/pressure/memory`.

**Acceptance Scenarios**:

1. **Given** macOS, **Then** the title shows `normal`, `warn` or `critical` from the kernel's
   pressure level, and the swap row shows swap-in/out per second.
2. **Given** Linux with PSI, **Then** the title shows `pressure N%` (the `some avg10` value);
   without PSI, the chip is absent and the explain entry says PSI is not enabled.
3. **Given** Kubernetes pod or node mode, **Then** the values come from the node's
   `/proc/vmstat` and `/proc/pressure/memory`, read through the existing reader.

---

### User Story 3 - Load average graph and run queue (Priority: P3)

The system card shows the run queue (`runq R/T`: runnable / total tasks) next to the load
averages and a graph of the 1-minute load scaled to the core count. The explain entry says load
is not CPU%, and on Linux counts tasks in D state.

**Why this priority**: teaches that load is a queue length, visibly, next to the CPU graph.

**Independent Test**: compare with `cat /proc/loadavg` (Linux) or `sysctl vm.loadavg` and
`uptime` (macOS).

**Acceptance Scenarios**:

1. **Given** Linux, **Then** `runq` shows fields 4 of `/proc/loadavg`.
2. **Given** macOS (no run-queue count in `vm.loadavg`), **Then** `runq` is counted from
   process states and its provenance says so.
3. **Given** load above the core count, **Then** the graph saturates in the top colour.

---

### User Story 4 - Network totals, address and link speed (Priority: P4)

The network card title names the main interface, its IPv4 address and link speed; each head line
adds the total bytes since the counters started.

**Why this priority**: useful context, smaller lesson (addresses belong to interfaces; counters
reset when an interface comes up).

**Independent Test**: compare with `ifconfig`/`ip addr`, `networksetup -getmedia` or
`ethtool`/`/sys/class/net/*/speed`, and `netstat -ib` or `/proc/net/dev`.

**Acceptance Scenarios**:

1. **Given** Wi-Fi or Ethernet as the default route's interface, **Then** the title shows it,
   its IPv4 address and speed when the platform reports one.
2. **Given** no reported speed (Wi-Fi on Linux, veth, `-1`), **Then** speed is left out.
3. **Given** Kubernetes mode, **Then** totals show; the address and speed are left out (the
   `net_link` lesson says why).

### Edge Cases

- Mount points longer than the column are truncated at the end with `…`, as elsewhere in rtop.
- More volumes than rows: the last row reads `N more · v to list`.
- A volume of size 0 (autofs, map points) is skipped.
- Counters that go backwards (interface reset, wrap) produce a rate of 0, not a spike.
- The first sample has no rate: show nothing until the second.
- No swap configured: the swap row keeps its current look, rates show `0`.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The disk card MUST show volumes with used %, fullest first, as many as fit, and a
  `v` key MUST list every volume (mount point, type, device, size, used, free).
- **FR-002**: Space MUST be computed as `df` does: size = `blocks × frsize`, used = `(blocks −
  bfree) × frsize`, free = `bavail × frsize` (the gap is the root reserve), used % = used /
  (used + free). On macOS, APFS volumes share their container's space: rtop MUST show one row
  per container, named by its browsable mount point (`/`), with used = size − free across all
  its volumes. `df` says `/` is 43% used on a container that is 98% full, because the user's
  data sits on the hidden `/System/Volumes/Data`; the explain entry teaches this.
- **FR-002a**: macOS hides containers whose volumes are all `nobrowse` (`MNT_DONTBROWSE`:
  Preboot, VM, xarts, simulator images); the `v` list still shows them.
- **FR-003**: Volume collection MUST NOT block the UI: macOS uses `getfsstat(MNT_NOWAIT)`;
  Linux skips network and FUSE filesystems or reads them off the UI thread with a timeout. Space
  is refreshed at most every 5 s.
- **FR-004**: Pseudo filesystems MUST be hidden, and a device mounted at several points MUST be
  shown once (its shortest mount point).
- **FR-005**: The memory card MUST show swap-in/out pages per second, derived from two samples
  of the kernel counters, and memory pressure where the platform reports it.
- **FR-006**: The system card MUST show runnable/total tasks and a two-row graph of the 1-minute
  load scaled so that the core count is the top.
- **FR-007**: The network card MUST show totals since the counters started; on the local machine
  also the main interface's name, IPv4 address and link speed when known.
- **FR-008**: Every new value MUST carry provenance and an `assets/explain.toml` entry; Linux
  parsers MUST be pure and fixture-tested.
- **FR-009**: Where a value can't be read (platform, Kubernetes mode, kernel option missing),
  the card MUST say so or leave the item out with the reason in the explain entry; never panic.

### Key Entities

- **Volume**: mount point, device, filesystem type, size, used, free, `answering` flag.
- **MemPressure**: swap-in and swap-out page counters, page size, pressure (level word or PSI
  `some avg10`).
- **RunQueue**: runnable, total, provenance.
- **NetLink**: interface name, IPv4 address, link speed in Mb/s, byte totals.

### Platform matrix

| Value | macOS | Linux | k8s pod / node |
|---|---|---|---|
| volumes | `getfsstat(MNT_NOWAIT)` (`statfs`) | `/proc/self/mountinfo` + `statvfs` | not shown: `statvfs` is a syscall the reader can't make |
| swap in/out | `host_statistics64(HOST_VM_INFO64)` `swapins`/`swapouts` | `/proc/vmstat` `pswpin`/`pswpout` | node `/proc/vmstat` |
| pressure | `kern.memorystatus_vm_pressure_level` (1/2/4) | `/proc/pressure/memory` `some avg10` | node `/proc/pressure/memory` |
| run queue | counted from process states (derived) | `/proc/loadavg` field 4 | node `/proc/loadavg` |
| totals | `NET_RT_IFLIST2` (64-bit) | `/proc/net/dev` | `/proc/net/dev` (pod) or `/proc/1/net/dev` (node) |
| address | `getifaddrs` | `getifaddrs` | not shown |
| link speed | `if_data64.ifi_baudrate` | `/sys/class/net/IF/speed` | not shown |
| main interface | first active `en*` with IPv4 (`getifaddrs`); under a VPN the default route is a `utun` | first `00000000` destination in `/proc/net/route` | — |

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Free space matches `df -k` Available, and on Linux used matches `df -k` Used, to
  within one block for every listed volume.
- **SC-002**: Swap rates match `vm_stat 1`/`vmstat 1` over the same interval to within one
  sample; pressure matches the sysctl or PSI file.
- **SC-003**: Idle CPU stays about 1% (constitution V) with volumes enabled.
- **SC-004**: With a stopped NFS server (or `sleep`-blocked FUSE mount), the UI still answers
  keys within one tick.

## Assumptions

- "Totals since boot" in the roadmap is really "since the interface's counters started"; the
  label says so.
- The main interface is the default route's; other interfaces are not listed (a later feature).
- The `v` list is an overlay like help, closed with Esc or `v`.
