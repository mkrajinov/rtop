---

description: "Task list for 001-k8s-targets"
---

# Tasks: Run rtop against Kubernetes pods and nodes

**Input**: Design documents from `/specs/001-k8s-targets/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: included. Constitution Principle IV requires unit tests for pure logic (wire parser,
attribution, kubectl JSON) and insta snapshots for UI changes, rendered from fixtures.

**Organization**: tasks are grouped by user story so each story can be built and tested on its
own.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: US1–US4 from spec.md
- All cluster commands use `--context kind-rtop` only

---

## Phase 1: Setup

**Purpose**: dependency, module skeleton, local cluster tooling

- [ ] T001 Add `serde_json = "1"` to `[dependencies]` in Cargo.toml
- [ ] T002 [P] Create module skeleton `src/kube/mod.rs` with empty `kubectl`, `session`, `wire`, `attribution`, `ephemeral`, `helper` submodules, and register `pub mod kube;` in src/lib.rs
- [ ] T003 [P] Add `kube-up`, `kube-e2e`, `kube-down` targets to Makefile (kind cluster `rtop`, context `kind-rtop` hard-coded, `kubectl apply -f tests/kube/workloads.yaml`)
- [ ] T004 [P] Write sample workloads `multi`, `distroless`, `shared` and `limited`, in namespace `rtop-demo` labelled `pod-security.kubernetes.io/enforce=privileged`, in tests/kube/workloads.yaml (per quickstart.md §1)

---

## Phase 2: Foundational (blocking prerequisites)

**Purpose**: identity, non-blocking details and the wire format, which every story needs.
Local mode must behave exactly as before.

**⚠️ CRITICAL**: no user story work begins until this phase is complete

- [ ] T005 Add `ProcId { space: u16, pid: Pid }`, `Process.space`, `Process.origin: Option<Origin>`, `Process::id()` and `Target` to src/model.rs (data-model.md)
- [ ] T006 Change `Collector::details` to take `ProcId`, and add `fn target(&self) -> Option<&Target>` (default `None`) and a `Pending` error type, in src/collector/mod.rs
- [ ] T007 Update src/collector/linux.rs, src/collector/macos.rs and src/collector/fixture.rs to set `space: 0`, `origin: None` and accept `ProcId` in `details`
- [ ] T008 Re-key src/app.rs on `ProcId` instead of `Pid`: `by_pid`, `children`, `selected_pid`, `Scope::Family`, the details request, the signal target and the lab scope set. `ppid` resolves within the same `space`.
- [ ] T009 Handle `Pending` from `details()` in src/app.rs and src/ui/detail.rs: show "loading…" and retry on the next refresh, without a status-line error
- [ ] T010 Run `cargo test`: existing insta snapshots in tests/snapshots/ must be unchanged (local mode untouched)
- [ ] T011 [P] Write fixture captures tests/fixtures/kube/hello.bin, snap-pod.bin, snap-node.bin, details.bin (frames per contracts/reader-wire.md), from kind once T018 works; hand-written placeholders first
- [ ] T012 [P] Implement the pure frame parser in src/kube/wire.rs: `\x1eF`/`\x1eE`/`\x1eD` framing, `p <pid>` records, link lines, the truncated-frame rule, seq matching
- [ ] T013 [P] Unit tests for src/kube/wire.rs against tests/fixtures/kube/*.bin: well-formed, unreadable file (rc≠0), `\x1e` inside cmdline, stale seq discarded

**Checkpoint**: local rtop works as before; `ProcId` is in place; the wire format is parsed and
tested on macOS.

---

## Phase 3: User Story 1 - Inspect all processes of a pod (Priority: P1) 🎯 MVP

**Goal**: `rtop --pod ns/name` shows every container's processes with a CONTAINER column;
exec first, debug container fallback.

**Independent Test**: quickstart.md §2–§4 on kind.

### Tests for User Story 1

- [ ] T014 [P] [US1] Unit tests for pod JSON → `PodInfo` (containers, statuses, ephemeral containers, shareProcessNamespace) against tests/fixtures/kube/pod.json in src/kube/kubectl.rs
- [ ] T015 [P] [US1] Snapshot test in tests/ui.rs: pod-mode fixture with containers `app` and `sidecar`, two PID 1 rows, CONTAINER column and target header line
- [ ] T016 [P] [US1] Unit test in src/collector/kube.rs: merging two session samples gives distinct `ProcId`s, and the tree does not link across spaces

### Implementation for User Story 1

- [ ] T017 [US1] Implement the kubectl wrapper in src/kube/kubectl.rs: resolve the binary (`--kubectl`), resolve the context once, `get_json`, `spawn_exec`, always passing `--context`; map stderr to FR-012 messages (forbidden, not found, unreachable)
- [ ] T018 [US1] Write the POSIX sh reader in src/kube/reader.sh implementing `hello`, `snap`, `cmd`, `details`, `bye` per contracts/reader-wire.md; uid from the `Uid:` line, fd count via `set -- /proc/$p/fd/*`
- [ ] T019 [US1] Implement Session in src/kube/session.rs: spawn `kubectl exec -i … -- sh`, send reader.sh, a worker thread that sends `snap` each tick, keeps `latest`, a state machine Probing→Starting→Live→Stale with backoff 1–30 s, and a `details` request with a 5 s timeout
- [ ] T020 [US1] Build `LinuxEnv` and the uid→name map from `hello` frames (passwd, pagesize, btime; ticks = 100) in src/kube/session.rs
- [ ] T021 [US1] Implement KubeCollector for pods in src/collector/kube.rs: one session per running container; `snapshot()` merges the latest samples via `linux_parse::process_row` and `linux_parse::system`; fetches cmdlines for new `(ProcId, starttime)` through `cmd`; sets `origin.container`
- [ ] T022 [US1] `details(ProcId)` in src/collector/kube.rs: route to the session, return `Pending` while in flight, build `ProcFiles` from frames, call `linux_parse::details`, prefix each provenance with `[pod/container]` and add the session's kubectl command (research R11)
- [ ] T023 [US1] Add flags `--pod`, `-c/--container`, `-n/--namespace`, `--context`, `--kubectl` and the Kubernetes default `-d` of 2.0 to src/main.rs, per contracts/cli.md; reject `lab` in Kubernetes mode
- [ ] T024 [US1] CONTAINER column (shown only when a target is set) and a container filter key in src/ui/table.rs and src/app.rs (FR-005)
- [ ] T025 [US1] Target line `context · ns/pod · node` and a stale marker with age in src/ui/header.rs (FR-008, FR-012)
- [ ] T026 [US1] Probe and fallback in src/kube/ephemeral.rs: `command -v cat` probe; `kubectl debug --target --profile=general --image <debug-image> -c rtop-<c>-<rand> -- sleep 3600`; reuse a running `rtop-<c>-*`; `--debug-image` and `--no-debug-container` flags; user notice (FR-003)
- [ ] T027 [US1] `shareProcessNamespace` in src/collector/kube.rs: a single session, with container attribution via `/proc/<pid>/cgroup` matched to `containerStatuses[].containerID`, and a `pause` label (research R4)
- [ ] T028 [US1] Handle pod gone, restarted (new restart count) and not Running, with a reattach offer, in src/collector/kube.rs and src/app.rs (spec edge cases)
- [ ] T029 [US1] Read-only by default: disable `k` and `+`/`-` unless `--allow-actions`; with it, send `kill`/`renice` requests through the session and confirm with the target named, in src/app.rs (FR-011)
- [ ] T030 [US1] Validate quickstart.md §2–§4 on kind; replace the placeholder fixtures in tests/fixtures/kube/ with real captures

**Checkpoint**: Story 1 works on kind with multi-container, distroless and shared-namespace pods.

---

## Phase 4: User Story 2 - Inspect all processes of a node (Priority: P2)

**Goal**: `rtop --node name` shows every node process with POD/CONTAINER columns, through a
labelled, self-expiring helper pod.

**Independent Test**: quickstart.md §5 on kind.

### Tests for User Story 2

- [ ] T031 [P] [US2] Write tests/fixtures/kube/cgroup-paths.txt (systemd and cgroupfs drivers; containerd, cri-o and docker; `/..`-prefixed paths; host processes) and tests/fixtures/kube/pods-on-node.json
- [ ] T032 [P] [US2] Unit tests for src/kube/attribution.rs against cgroup-paths.txt, including pod UID `_`→`-` and no match → `host`
- [ ] T033 [P] [US2] Unit test that the helper manifest renders as contracts/helper-pod.yaml (labels, deadline, hostPID, SYS_PTRACE, read-only cgroup mount) in src/kube/helper.rs
- [ ] T034 [P] [US2] Snapshot test in tests/ui.rs: node-mode fixture with POD and CONTAINER columns and `host` rows

### Implementation for User Story 2

- [ ] T035 [US2] Implement the pure `attribution::parse(path) -> Option<(pod_uid, container_id)>` in src/kube/attribution.rs (research R9)
- [ ] T036 [US2] Helper pod lifecycle in src/kube/helper.rs:
  - create with `kubectl create -f -`;
  - wait for Running (60 s; on failure show events and the admission message);
  - delete on exit, Ctrl-C and SIGTERM;
  - delete finished helpers found by label at start;
  - `RTOP_HELPER_DEADLINE` override;
  - `--helper-namespace` flag.
- [ ] T037 [US2] NodeIndex in src/collector/kube.rs: `get pods -A --field-selector spec.nodeName=` refreshed every 10 s or when an unknown ID appears; `cg` requests for new PIDs; set `origin.pod`/`origin.container`
- [ ] T038 [US2] Node session in src/collector/kube.rs: user names from `/proc/1/root/etc/passwd`, `[node/host]` provenance, node system cards, plus allocatable capacity and pod count from `kubectl get node -o json` (FR-010)
- [ ] T039 [US2] `--node` flag in src/main.rs and the POD column in src/ui/table.rs; `/` filter matches the pod name
- [ ] T040 [US2] Validate quickstart.md §5 on kind, including no leftover helper after quit and expiry with `RTOP_HELPER_DEADLINE=60`; capture snap-node.bin

**Checkpoint**: Stories 1 and 2 both work independently.

---

## Phase 5: User Story 3 - Choose a target interactively (Priority: P3)

**Goal**: `rtop --kube` opens a filterable pod/node picker.

**Independent Test**: quickstart: start `rtop --context kind-rtop --kube -n rtop-demo`, pick
`multi`, quit, pick the node.

- [ ] T041 [P] [US3] Snapshot test in tests/ui.rs: picker with a pods tab and a nodes tab from fixtures, filter applied
- [ ] T042 [US3] Implement the picker in src/ui/picker.rs and src/app.rs: pods (status, node, container count), nodes (Ready, roles, pod count), `/` filter, Enter attaches, Tab switches lists
- [ ] T043 [US3] For a non-Running pod or NotReady node, explain why it can't attach instead of attaching, in src/ui/picker.rs
- [ ] T044 [US3] `--kube` flag in src/main.rs; switch from the picker to KubeCollector without restarting the terminal

**Checkpoint**: Stories 1–3 work.

---

## Phase 6: User Story 4 - Container-aware dashboard and lessons (Priority: P4)

**Goal**: each container's usage against its limits, throttling, OOM kills and restarts, plus
the lessons in the explain panel.

**Independent Test**: quickstart.md §6 with the `limited` pod.

- [ ] T045 [P] [US4] Unit tests in src/collector/linux_parse.rs for new parsers of `cpu.stat`, `memory.events` and `cpu.max`
- [ ] T046 [P] [US4] Snapshot test in tests/ui.rs: container usage card with limits, throttling and an OOM count
- [ ] T047 [US4] Add parsers `cgroup_cpu_stat`, `cgroup_memory_events` and `cgroup_cpu_max` to src/collector/linux_parse.rs
- [ ] T048 [US4] `snap` cgroup root: `/sys/fs/cgroup` for exec sessions, `/proc/<target pid 1>/root/sys/fs/cgroup` for debug sessions, with a fallback note when unreadable, in src/kube/reader.sh and src/collector/kube.rs (research R7)
- [ ] T049 [US4] `ContainerUsage` with CPU % of the `cpu.max` quota and memory % of the limit, plus requests and limits from PodInfo, in src/collector/kube.rs
- [ ] T050 [US4] Container usage card replacing the battery or GPU slot in pod mode, with sources in the title, in src/ui/header.rs
- [ ] T051 [US4] Explain entries in assets/explain.toml: `container`, `pod`, `k8s.limits`, `k8s.requests`, `cgroup.cpu_throttled`, `cgroup.oom_kill`, `k8s.restarts`, `k8s.ephemeral_container`, `k8s.host_pid`, and "meminfo in a container shows the node"
- [ ] T052 [US4] Validate quickstart.md §6 on kind

**Checkpoint**: all stories work independently.

---

## Phase 7: Polish & cross-cutting

- [ ] T053 [P] README.md: Kubernetes mode section (flags, modes, what gets added to the cluster and for how long), permissions from contracts/rbac.yaml, new sources-table rows
- [ ] T054 [P] ROADMAP.md: tick the Kubernetes targets items
- [ ] T055 Measure target CPU (quickstart.md §8). If it is above 10m, apply research R2's mitigations in src/kube/reader.sh and record the result in research.md.
- [ ] T056 Run quickstart.md §7 failure table; every case shows a cause line, with no panic (SC-005)
- [ ] T057 Run `make test lint` (macOS) and `make linux-test` (Docker); all green, and snapshot changes reviewed
- [ ] T058 Check rtop idle CPU in pod mode (about 1% locally) with the existing flood harness approach

---

## Dependencies & Execution Order

### Phase dependencies

- **Setup (1)**: no dependencies.
- **Foundational (2)**: after Setup. Blocks all stories. T005 → T006 → T007 → T008 → T009 → T010
  run in sequence. T011–T013 can run in parallel with them.
- **US1 (3)**: after Foundational. It is the MVP and provides kubectl, Session and KubeCollector.
- **US2 (4)**: after US1's T017–T022 (it reuses the wrapper, session and collector). It does not
  need T024–T029.
- **US3 (5)**: after US1 (attaches to pods). The nodes tab needs US2.
- **US4 (6)**: after US1. Independent of US2 and US3.
- **Polish (7)**: after the stories you ship.

### Within US1

T017 → T018 → T019 → T020 → T021 → T022 → T023, then T024–T029 (mostly separate files), then T030.

### Parallel opportunities

- Setup: T002, T003, T004.
- Foundational: T011, T012, T013, alongside the T005–T010 chain.
- US1 tests T014–T016 together before implementation.
- US2 tests T031–T034 together; T035 can start in parallel with US1's tail (pure function).
- US4 T045–T047 (parsers and tests) can start right after Foundational.

### Parallel example: User Story 2

```text
T031 fixtures cgroup-paths.txt + pods-on-node.json
T032 attribution tests          (after T031)
T033 helper manifest test
T034 node-mode snapshot test
```

## Implementation Strategy

### MVP first (User Story 1)

1. Phases 1–2, verifying that local snapshots are unchanged.
2. Phase 3 up to T025: exec-only pod view, which is already useful on images with a shell.
3. T026–T029: fallback, shared namespace, edge cases, actions.
4. **Stop and validate** with quickstart §2–§4. Commit and push.

### Incremental delivery

1. MVP (US1), then US2 (nodes), then US4 (dashboard lessons), then US3 (picker).
   US4 is ranked above US3 for learning value (constitution: teach first).
2. Each story ends with its quickstart section on kind and `make test lint` green.

## Notes

- Never run cluster commands without `--context kind-rtop` while developing; the current
  context is a production cluster.
- Commit after each task or small group with Conventional Commits (`feat(kube): …`,
  `test(kube): …`, `refactor(app): …`).
- `ProcId` (T008) is the riskiest refactor. Land it alone, with all existing snapshots unchanged.
