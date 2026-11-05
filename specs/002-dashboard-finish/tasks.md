# Tasks: Finish the dashboard

**Input**: [spec.md](spec.md), [plan.md](plan.md). Each story is committed on its own after
`make test lint` (and `make linux-test` for Linux parsing) pass.

## Phase 1: Shared

- [x] T001 Extend `L` to local mode: `DASH_LESSONS` in `src/app.rs`, help text in `src/ui/mod.rs`

## Phase 2: User Story 1 — volumes (P1)

- [x] T010 [US1] `Volume`, `Volumes` in `src/model.rs`; `SystemInfo.volumes`
- [x] T011 [US1] Pure `space()` and `parse_mountinfo()` + `volumes_from_mounts()` in `src/collector/linux_parse.rs`, tests with `tests/fixtures/linux/mountinfo`
- [x] T012 [US1] Linux `statvfs` collection with 5 s cache in `src/collector/linux.rs`
- [x] T013 [US1] macOS `getfsstat(MNT_NOWAIT)` with APFS container grouping and 5 s cache in `src/collector/macos.rs`; pure grouping function tested
- [x] T014 [US1] Fixture volumes in `src/collector/fixture.rs`
- [x] T015 [US1] Disk card split and `v` overlay in `src/ui/header.rs`, `src/ui/mod.rs`, `src/app.rs`; Kubernetes message
- [x] T016 [US1] Explain `volumes`, `apfs_container`; README sources and keys; snapshots; ROADMAP tick

## Phase 3: User Story 2 — swap activity and pressure (P2)

- [x] T020 [US2] `SwapIo`, `Pressure` in model; parsers `vmstat_swap()`, `psi()` + fixtures
- [x] T021 [US2] Linux and macOS collection; `reader.sh` reads `/proc/vmstat`, `/proc/pressure/memory`; kube collector parses them
- [x] T022 [US2] Swap rate in `src/app.rs`; memory card title chip and swap row
- [x] T023 [US2] Explain `swap_activity`, `mem_pressure`; README; snapshots; ROADMAP tick

## Phase 4: User Story 3 — run queue and load graph (P3)

- [x] T030 [US3] `run_queue` from `/proc/loadavg` in `linux_parse::system`; test
- [x] T031 [US3] `load_hist` in app; system card rows; macOS derived run queue
- [x] T032 [US3] Explain `run_queue`, `load_graph`; README; snapshots; ROADMAP tick

## Phase 5: User Story 4 — network totals, address, speed (P4)

- [ ] T040 [US4] `NetLink` in model; `default_route_iface()` parser for `/proc/net/route` + fixture
- [ ] T041 [US4] Linux and macOS collection (`getifaddrs`, speed)
- [ ] T042 [US4] Network card title and totals in head lines
- [ ] T043 [US4] Explain `net_link`; README; snapshots; ROADMAP tick

## Phase 6: Polish

- [ ] T050 `make linux-test` in Docker; check PSI presence there
- [ ] T051 Idle CPU check (SC-003) and a screenshot of the dashboard
