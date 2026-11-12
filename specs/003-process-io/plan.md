# Implementation Plan: Per-process I/O columns

**Spec**: [spec.md](spec.md)

## Design

- **Model**: `Process.io: Option<[u64; 2]>` (cumulative storage bytes read, written; `None`
  when not read or not readable) and `Process.io_rate: Option<[f32; 2]>` (bytes/s, filled by
  the tracker from the second sample).
- **Collector**: `Collector::set_io(&mut self, on: bool)`, default no-op. macOS and Linux
  collectors read I/O only when on. Kubernetes ignores it (rows stay `None`).
- **Linux**: pure `linux_parse::io_bytes(text) -> Option<[u64; 2]>` (`read_bytes`,
  `write_bytes`), tested against `tests/fixtures/linux/<pid>/io`.
- **macOS**: existing `rusage_v4(pid)` → `ri_diskio_bytesread`, `ri_diskio_byteswritten`.
- **Rates**: `IoTracker` in `collector/cpu.rs`, keyed by `(ProcId, start_time)` like
  `CpuTracker`; `saturating_sub`; first sample `None`.
- **App**: `show_io: bool`; `i` toggles it and calls `set_io`; `columns()` adds
  `Column::Read`, `Column::Write` after RSS when on; status message in Kubernetes mode.
- **UI**: `table.rs` cells in the same position as `Column::ALL`; rate values, zero faint, `—`
  for `None` io, blank before the second sample; the disk card's read and write colours. Help
  and footer gain `i`.
- **Explain**: `Column::Read → io_read`, `Column::Write → io_write`; both entries extended.

## Constitution Check

I provenance (sources in explain and README), II teach (io entries), III both platforms plus a
Kubernetes message, IV pure parser, tracker unit test, snapshot, V read only when shown. No
violations.
