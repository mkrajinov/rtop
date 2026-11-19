# Feature Specification: `pagecache` lab

**Feature Branch**: `main`

**Created**: 2026-11-19

**Status**: Implemented

**Input**: ROADMAP Phase 4, `pagecache`: the second read of a file never touches the disk. The
other half of the per-process I/O lesson (spec 003).

## User Story (P1)

`rtop lab pagecache` starts two readers, `pagecache-cold` and `pagecache-warm`, that read a
256 MiB file each at the same pace (64 MiB/s, over and over). The I/O columns are on. The cold
reader shows ~64M READ/s, the warm one 0; on Linux both show the same `rchar` growth. Six steps
walk through storage vs syscall bytes, how files enter and leave the cache, deleted-but-open
files, where cached memory is counted (cgroups, kubelet working set), and benchmarking the disk.

**Acceptance**:

1. Given macOS or Linux, the cold reader's READ/s is about 64M and the warm reader's is 0.
2. Given Linux, `/proc/PID/io` `rchar` grows at the same rate for both; `read_bytes` only for
   the cold one.
3. When the lab ends (quit, hang-up, 10-minute limit), no files are left: they were unlinked
   right after creation.

## Design

- The worker writes both files, `fsync`s them, unlinks them, and starts each reader as
  `rtop __lab-worker pagecache-cold|warm` with its file as stdin, so the COMMAND column names
  the reader and the Files tab shows a deleted file.
- Cold: Linux `posix_fadvise(POSIX_FADV_DONTNEED)` after writing and after every pass (no root
  needed); macOS `F_NOCACHE` on the file.
- Readers `pread` 8 MiB chunks paced to 64 MiB/s, so every 1-second sample sees reads.
- `App::with_lab` turns the I/O columns on for this lab.

## Verification

- macOS (APFS): cold 63.9M READ/s, warm 0.
- Linux (Docker, overlayfs): over 2 s, cold `read_bytes` +128 MiB, warm `read_bytes` 0, both
  `rchar` +128 MiB; `fd/0` → `… (deleted)`.
