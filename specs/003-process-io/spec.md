# Feature Specification: Per-process I/O columns

**Feature Branch**: `main` (commits per step)

**Created**: 2026-11-12

**Status**: Draft

**Input**: ROADMAP Phase 2, "Per-process I/O columns (read/s, write/s)": teaches `rchar` vs
`read_bytes`, and that page cache hits never reach the disk.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Which process is using the disk (Priority: P1)

The disk card says the machine reads 80 MB/s; `i` adds READ/s and WRITE/s columns to the
process table so the user can sort by them and see which process it is. The explain entries say
what the numbers count: bytes the process made the storage layer move, not bytes passed to
`read()`; a process re-reading a cached file shows 0.

**Independent Test**: `dd if=/dev/zero of=big bs=1m count=2000` (macOS) or
`dd if=/dev/zero of=big bs=1M count=2000 oflag=direct` (Linux), press `i`, sort by WRITE/s: `dd`
is on top, and its rate matches `dd`'s own report within one sample.

**Acceptance Scenarios**:

1. **Given** the table, **When** the user presses `i`, **Then** READ/s and WRITE/s appear after
   RSS, empty on the first sample, and filled from the second; `i` again hides them.
2. **Given** a process rtop may not inspect (another user's on Linux without root, or a
   restricted one on macOS), **Then** its cells show `—`, as other unreadable cells do.
3. **Given** Kubernetes mode, **When** the user presses `i`, **Then** the columns show `—` and
   the status line says per-process I/O is not read there (the explain entry says why).
4. **Given** the columns are hidden, **Then** rtop reads no per-process I/O (idle cost unchanged).

### Edge Cases

- A recycled PID never inherits the old process's counters (keyed by start time).
- Counters that go backwards read as 0, not a spike.
- Narrow terminals: the I/O columns are dropped last, after PRI, VSZ, TIME+, FD, THR and USER.

## Requirements *(mandatory)*

- **FR-001**: `i` MUST toggle READ/s and WRITE/s columns, sortable like every numeric column,
  in the disk card's order (read, then write) and colours.
- **FR-002**: Rates MUST be storage bytes per second from two samples of the cumulative
  counters: Linux `read_bytes`/`write_bytes` in `/proc/PID/io`, macOS
  `rusage_info.ri_diskio_bytesread`/`ri_diskio_byteswritten` from `proc_pid_rusage`.
- **FR-003**: Per-process I/O MUST only be read while the columns are shown.
- **FR-004**: The Linux parser MUST be pure and fixture-tested; the explain entries `io_read`,
  `io_write` MUST cover the rates, when Linux charges writes (when pages are dirtied, to the
  dirtying process), and Kubernetes mode.

## Platform matrix

| | macOS | Linux | k8s pod / node |
|---|---|---|---|
| read, write | `proc_pid_rusage(RUSAGE_INFO_V4)` `ri_diskio_*` | `/proc/PID/io` `read_bytes`, `write_bytes` | not read: one more file per process per second through the reader shell; a later feature |
| other users | EPERM → `—` | ptrace-gated, EACCES → `—` unless root | — |

## Success Criteria

- **SC-001**: A `dd` writing at a steady rate tops WRITE/s with its own rate ±1 sample.
- **SC-002**: Idle CPU unchanged with the columns hidden; with them shown, under 1.5×.

## Assumptions

- Logical bytes (`rchar`/`wchar`) stay in the detail view; the contrast is the `pagecache`
  lab's job (a later feature).
