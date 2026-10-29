---

description: "Task list for 001-k8s-targets"
---

# Tasks: Run rtop against Kubernetes pods and nodes

**Input**: Design documents from `/specs/001-k8s-targets/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: included. Constitution Principle IV requires unit tests for pure logic (wire parser,
attribution, k8s object conversion, kube thread against a mocked API) and insta snapshots for UI changes, rendered from fixtures.

**Organization**: tasks are grouped by user story so each story can be built and tested on its
own.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: US1–US4 from spec.md
- All cluster commands use `--context kind-rtop` only

---

## Phase 1: Setup

**Purpose**: feature-gated dependencies, module skeleton, local cluster tooling

- [X] T001 Add the `[features] default = ["kube"]` feature to Cargo.toml, with optional deps `kube` (`client`, `runtime`, `ws`, `rustls-tls`), `k8s-openapi` (pinned API version), `tokio` (`rt`, `macros`, `time`, `io-util`, `sync`), `futures` and `serde_json`, and dev-deps `tower-test` and `http`
- [X] T002 [P] Create the module skeleton `src/kube/`: `msg`, `client`, `convert`, `ephemeral`, `helper` and `session` under `#[cfg(feature = "kube")]`; `wire` and `attribution` always compiled. Register `pub mod kube;` in src/lib.rs.
- [X] T003 [P] Makefile: `kube-up`, `kube-e2e` and `kube-down` targets (kind cluster `rtop`, context `kind-rtop` hard-coded); `test` also runs `cargo test --no-default-features`
- [X] T004 [P] Write sample workloads `multi`, `distroless`, `shared` and `limited`, in namespace `rtop-demo` labelled `pod-security.kubernetes.io/enforce=privileged`, in tests/kube/workloads.yaml (per quickstart.md §1)

---

## Phase 2: Foundational (blocking prerequisites)

**Purpose**: identity, non-blocking details and the wire format, which every story needs.
Local mode must behave exactly as before.

**⚠️ CRITICAL**: no user story work begins until this phase is complete

- [X] T005 Measure the clean build time and binary size with and without `--no-default-features`, and record them in research.md R1 (first run 2026-10-29: release build 58 s vs 20 s, binary 1.9 MB both, because the kube code is not reachable from `main` yet; re-measure after T027). Done: 65 s and 10.8 MB with the feature, 19 s and 1.95 MB without, recorded in R1
- [X] T006 Add `ProcId { space: u16, pid: Pid }`, `Process.space`, `Process.origin: Option<Origin>`, `Process::id()` and `Target` to src/model.rs (data-model.md)
- [X] T007 Change `Collector::details` to take `ProcId`, and add `fn target(&self) -> Option<&Target>` (default `None`) and a `Pending` error type, in src/collector/mod.rs
- [X] T008 Update src/collector/linux.rs, src/collector/macos.rs and src/collector/fixture.rs to set `space: 0`, `origin: None` and accept `ProcId` in `details`
- [X] T009 Re-key src/app.rs on `ProcId` instead of `Pid`: `by_pid`, `children`, `selected_pid`, `Scope::Family`, the details request, the signal target and the lab scope set. `ppid` resolves within the same `space`.
- [X] T010 Handle `Pending` from `details()` in src/app.rs and src/ui/detail.rs: show "loading…" and retry on the next refresh, without a status-line error
- [X] T011 Run `cargo test`: existing insta snapshots in tests/snapshots/ must be unchanged (local mode untouched)
- [X] T012 [P] Write fixture captures tests/fixtures/kube/hello.bin, snap-pod.bin, snap-node.bin, details.bin (frames per contracts/reader-wire.md), from kind once T022 works; hand-written placeholders first
- [X] T013 [P] Implement the pure frame parser in src/kube/wire.rs: `\x1eF`/`\x1eE`/`\x1eD` framing, `p <pid>` records, link lines, the truncated-frame rule, seq matching
- [X] T014 [P] Unit tests for src/kube/wire.rs against tests/fixtures/kube/*.bin: well-formed, unreadable file (rc≠0), `\x1e` inside cmdline, stale seq discarded
- [X] T015 Define `Request`, `Reply`, `Event`, `KubeError` and the shapes in src/kube/msg.rs, per contracts/kube-messages.md
- [X] T016 Kube thread skeleton in src/kube/mod.rs:
  - spawn a thread with a tokio `current_thread` runtime;
  - `std::sync::mpsc` channels in both directions;
  - `catch_unwind` with restart and `Event::ThreadRestarted`;
  - `Shutdown` handling;
  - the `Hello` handler in src/kube/client.rs (`Config::from_kubeconfig` with the context, server version).
- [X] T017 [P] Test harness in src/kube/client.rs tests: a `tower_test::mock` service behind `kube::Client::new` with helpers to expect requests and answer them with fixture JSON; a test for `Hello` and for 401/403/404 → `KubeError` code mapping

**Checkpoint**: local rtop works as before, and `--no-default-features` builds. `ProcId` is in
place, the wire format is parsed and tested on macOS, and the kube thread answers `Hello`
against the mock.

---

## Phase 3: User Story 1 - Inspect all processes of a pod (Priority: P1) 🎯 MVP

**Goal**: `rtop --pod ns/name` shows every container's processes with a CONTAINER column;
exec first, debug container fallback.

**Independent Test**: quickstart.md §2–§4 on kind.

### Tests for User Story 1

- [X] T018 [P] [US1] Unit tests for `convert::pod_info` (k8s_openapi `Pod` → `PodInfo`: containers, statuses, ephemeral containers, shareProcessNamespace) against tests/fixtures/kube/pod.json and pod-shared.json in src/kube/convert.rs
- [X] T019 [P] [US1] Snapshot test in tests/ui.rs: pod-mode fixture with containers `app` and `sidecar`, two PID 1 rows, CONTAINER column and target header line
- [X] T020 [P] [US1] Unit test in src/collector/kube.rs: merging two session samples gives distinct `ProcId`s, and the tree does not link across spaces

### Implementation for User Story 1

- [X] T021 [US1] Pod handlers in src/kube/client.rs, each with a mock test from T017:
  - `GetPod`;
  - `WatchPod` (a `kube::runtime::watcher` emitting `Event::Pod` and `Event::PodDeleted`);
  - `ExecOpen`, `StreamWrite` and `StreamClose` via `Api::<Pod>::exec` (ws), writing `stdin_init` first and forwarding stdout as `Event::StreamData` and the end as `Event::StreamClosed` with the exit status;
  - stderr goes to the kube log ring buffer.
  - Done: `GetPod` and `WatchPod` have mock tests. Exec needs a WebSocket upgrade that
    `tower_test` cannot fake, so the session loop (`pump`) is tested on `tokio::io::duplex`
    pipes and the real exec was checked on kind. The last 512 bytes of stderr go into the
    `StreamClosed` error instead of a ring buffer.
- [X] T022 [US1] Write the POSIX sh reader in src/kube/reader.sh implementing `hello`, `snap`, `cmd`, `details`, `bye` per contracts/reader-wire.md; uid from the `Uid:` line, fd count via `set -- /proc/$p/fd/*`
- [X] T023 [US1] Sync side of a session in src/kube/session.rs:
  - send `ExecOpen` with reader.sh;
  - send `snap <seq>` each tick;
  - feed `StreamData` bytes to `wire.rs` and keep `latest`;
  - state machine Probing→Starting→Live→Stale with backoff 1–30 s;
  - `details` requests with a 5 s timeout.
- [X] T024 [US1] Build `LinuxEnv` and the uid→name map from `hello` frames (passwd, pagesize, btime; ticks = 100) in src/kube/session.rs
- [X] T025 [US1] Implement KubeCollector for pods in src/collector/kube.rs: one session per running container; `snapshot()` merges the latest samples via `linux_parse::process_row` and `linux_parse::system`; fetches cmdlines for new `(ProcId, starttime)` through `cmd`; sets `origin.container`
- [X] T026 [US1] `details(ProcId)` in src/collector/kube.rs: route to the session, return `Pending` while in flight, build `ProcFiles` from frames, call `linux_parse::details`, prefix each provenance with `[pod/container]` and add the equivalent `kubectl exec` command (research R11)
- [X] T027 [US1] Add flags `--pod`, `-c/--container`, `-n/--namespace`, `--context` and the Kubernetes default `-d` of 2.0 to src/main.rs, gated on `cfg(feature = "kube")`, per contracts/cli.md; reject `lab` in Kubernetes mode; send `Shutdown` on exit and from the panic hook
- [X] T028 [US1] CONTAINER column (shown only when a target is set) and a container filter key in src/ui/table.rs and src/app.rs (FR-005)
- [X] T029 [US1] Target line `context · ns/pod · node` (context highlighted when not given explicitly) and a stale marker with age in src/ui/header.rs (FR-008, FR-012)
- [X] T030 [US1] `Probe` and `EnsureDebugContainer` in src/kube/ephemeral.rs, with mock tests (reuse vs append, `Unsupported` when disabled):
  - probe with `command -v cat`, treating exit 127 or "not found" as missing;
  - reuse a running `rtop-<c>-*`, or patch the `ephemeralcontainers` subresource with `targetContainerName`, the debug image, `sleep 3600` and `SYS_PTRACE`;
  - wait for running;
  - `--debug-image` and `--no-debug-container` flags, and the user notice (FR-003).
  - Done differently: no separate `Probe` round trip. The exec of `sh` itself is the probe
    ("executable file not found", exit 127), and a hello whose `passwd` frame fails with 127
    means `cat` is missing. When policy refuses `SYS_PTRACE`, the container is added without it
    and the header says so. Disabled fallback is decided on the UI side (no request is sent).
- [X] T031 [US1] `shareProcessNamespace` in src/collector/kube.rs: a single session, with container attribution via `/proc/<pid>/cgroup` matched to `containerStatuses[].containerID`, and a `pause` label (research R4)
- [X] T032 [US1] Handle pod gone, restarted (new restart count) and not Running, with a reattach offer, in src/collector/kube.rs and src/app.rs (spec edge cases)
  - Done: reattaching is automatic. A restarted container gets a fresh reader. A deleted pod
    stops all readers, and a new pod with the same name (new UID) is picked up again.
- [X] T033 [US1] Read-only by default: disable `k` and `+`/`-` unless `--allow-actions`; with it, send `kill`/`renice` requests through the session and confirm with the target named, in src/app.rs (FR-011)
  - Done for signals (`kill <seq> <SIG> <pid>` in the reader; the result shows in the status
    line). Remote renice is left out for now and says so.
- [X] T034 [US1] Validate quickstart.md §2–§4 on kind; replace the placeholder fixtures in tests/fixtures/kube/ with real captures
  - Done: validated on kind with the multi, limited, shared and distroless pods. Real captures
    were added as `kind-*.bin`. The hand-written ones stay for edge cases a capture lacks.

**Checkpoint**: Story 1 works on kind with multi-container, distroless and shared-namespace pods.

---

## Phase 4: User Story 2 - Inspect all processes of a node (Priority: P2)

**Goal**: `rtop k8s --node name` shows every node process with POD/CONTAINER columns, through a
labelled, self-expiring helper pod.

**Independent Test**: quickstart.md §5 on kind.

### Tests for User Story 2

- [X] T035 [P] [US2] Write tests/fixtures/kube/cgroup-paths.txt (systemd and cgroupfs drivers; containerd, cri-o and docker; `/..`-prefixed paths; host processes) and tests/fixtures/kube/pods-on-node.json
  - Done: real lines from a hostPID busybox pod on kind (containerd, systemd driver, relative `/../..` paths, static-pod hashes) plus cri-o, docker, cgroupfs and Guaranteed-QoS paths; `pods-on-node.json` and `node.json` trimmed from the same cluster.
- [X] T036 [P] [US2] Unit tests for src/kube/attribution.rs against cgroup-paths.txt, including pod UID `_`→`-` and no match → `host`
  - Done: `fixture_paths` checks every line; `owners_on_a_node` covers pause via pod UID, `host`, and the helper's own `0::/`.
- [X] T037 [P] [US2] Unit test that the pure `helper::manifest()` builds the typed `Pod` from contracts/helper-pod.yaml (labels, deadline, hostPID, SYS_PTRACE, read-only cgroup mount), plus mock tests for create→Running, admission rejection→`Admission`, and GC by label, in src/kube/helper.rs
  - Done differently: no cgroup hostPath mount (nothing reads it yet; one host path fewer), `capabilities.drop: [ALL]` plus SYS_PTRACE, `automountServiceAccountToken: false`, and an `rtop.io/role: node-helper` label that GC selects on. GC test asserts a Running helper is left alone.
- [X] T038 [P] [US2] Snapshot test in tests/ui.rs: node-mode fixture with POD and CONTAINER columns and `host` rows
  - Done: `node_mode_shows_pods_and_host`, which also checks that `/multi` filters by pod and that `k` stays off.

### Implementation for User Story 2

- [X] T039 [US2] Implement the pure `attribution::parse(path) -> Option<(pod_uid, container_id)>` in src/kube/attribution.rs (research R9)
  - Done differently: `container_id()` and `pod_uid()` (from US1) plus `Owners::owner()`, which maps a cgroup to pod and container, falls back to the pod's `pause` by UID or config hash, and to `host`. Static pods carry a 32-hex config hash, matched through the mirror pod's `kubernetes.io/config.mirror` annotation.
- [X] T040 [US2] Helper pod lifecycle in src/kube/helper.rs (`HelperCreate`, `HelperDelete`, `HelperGc`):
  - create with `Api::<Pod>::create`;
  - wait for Running via a watcher (60 s; on failure list events and show the admission message);
  - delete on exit, Ctrl-C, SIGTERM and from the panic hook;
  - delete finished helpers found by label at start;
  - `RTOP_HELPER_DEADLINE` override;
  - `--helper-namespace` flag.
  - Done: create, wait for Running (image pull and stuck reasons reported), delete with grace 0, GC of finished helpers only. Deletion runs from `KubeCollector::drop`, so normal exit, Ctrl-C/SIGTERM and unwinding panics all delete; failures are printed after the TUI closes. When the helper ends (deadline, deleted), rtop deletes it and creates a new one. No event listing on failure: the admission message from the create call is shown instead.
- [X] T041 [US2] NodeIndex:
  - in src/kube/client.rs, `WatchNodePods` runs a watcher on pods with field selector `spec.nodeName=<node>`, kept in a map and sent as `Event::NodeIndex` debounced to 1 s, through the pure `convert::node_index`;
  - in src/collector/kube.rs, send `cg` requests for new PIDs and set `origin.pod`/`origin.container`.
  - Done differently: no `cg` request; the existing `cmd` request already returns each process's cgroup. Init and ephemeral container IDs are indexed too.
- [X] T042 [US2] Node session in src/collector/kube.rs: user names from `/proc/1/root/etc/passwd`, `[node/host]` provenance, node system cards, plus allocatable capacity and pod count from `GetNode` (FR-010)
  - Done: the reader gets `RTOP_ROOT=/proc/1/root` and `RTOP_NET=/proc/1/net` before the script, so passwd and network counters are the host's. Provenance is `[node <name>]`; the header shows allocatable CPU and memory and the pod count. Node mode is read-only even with `--allow-actions`: root in a hostPID pod can signal the kubelet, with or without capabilities.
- [X] T043 [US2] `--node` flag in src/main.rs and the POD column in src/ui/table.rs; `/` filter matches the pod name
  - Done: `rtop k8s --node NODE [--helper-namespace NS]`; POD column sized to the longest name; the filter matches the pod.
- [X] T044 [US2] Validate quickstart.md §5 on kind, including no leftover helper after quit and expiry with `RTOP_HELPER_DEADLINE=60`; capture snap-node.bin
  - Done: on kind, 53 processes in one snapshot (about 30 ms in the reader); no helper left after `q`, SIGTERM or Esc back to the picker; `RTOP_HELPER_DEADLINE=40` replaced the helper and removed the old one; a `baseline` namespace gives the admission message. Captured as `kind-node.bin` instead of `snap-node.bin`.

**Checkpoint**: Stories 1 and 2 both work independently.

---

## Phase 5: User Story 3 - Choose a target interactively (Priority: P3)

**Goal**: `rtop k8s` opens a filterable namespace/pod (later also node) picker.

**Independent Test**: quickstart: start `rtop k8s --context kind-rtop -n rtop-demo`, pick
`multi`, Esc back to the picker, pick another pod.

- [X] T045 [P] [US3] Snapshot test in tests/ui.rs: picker with a pods tab and a nodes tab from fixtures, filter applied
  - Done differently: `k8s_picker_lists_namespaces_and_pods` snapshots the namespace and pod panes; the filter is covered by unit tests in src/picker.rs. The nodes tab comes with US2.
- [X] T046 [US3] Implement the picker in src/ui/picker.rs and src/app.rs: pods (status, node, container count), nodes (Ready, roles, pod count), `/` filter, Enter attaches, Tab switches lists
  - Done differently (user request): two panes, namespaces (with "all namespaces") and their pods (READY, STATUS, RESTARTS, AGE, NODE). State and keys live in src/picker.rs rather than in App. New kube requests `ListNamespaces` and `ListPods` (empty ns = all). Forbidden namespace listing falls back to the context's namespace; `/` then Enter opens a typed one. Pods are listed again every 3 s. Nodes wait for US2.
- [X] T047 [US3] For a non-Running pod or NotReady node, explain why it can't attach instead of attaching, in src/ui/picker.rs
- [X] T048 [US3] `--kube` flag in src/main.rs; switch from the picker to KubeCollector without restarting the terminal
  - Done differently (user request): `rtop k8s` subcommand instead of `--kube`; `--pod`, `--context`, `-n`, `-c`, `--debug-image`, `--no-debug-container` and `--allow-actions` moved under it. The pod view opens on a thread while the picker keeps drawing (Esc cancels); its errors appear in the picker's status line. Esc in the pod view returns to the picker with that pod selected. Each pod view has its own kube thread, so leaving it stops its watchers and readers.

**Checkpoint**: Stories 1–3 work.

---

## Phase 6: User Story 4 - Container-aware dashboard and lessons (Priority: P4)

**Goal**: each container's usage against its limits, throttling, OOM kills and restarts, plus
the lessons in the explain panel.

**Independent Test**: quickstart.md §6 with the `limited` pod.

- [ ] T049 [P] [US4] Unit tests in src/collector/linux_parse.rs for new parsers of `cpu.stat`, `memory.events` and `cpu.max`
- [ ] T050 [P] [US4] Snapshot test in tests/ui.rs: container usage card with limits, throttling and an OOM count
- [ ] T051 [US4] Add parsers `cgroup_cpu_stat`, `cgroup_memory_events` and `cgroup_cpu_max` to src/collector/linux_parse.rs
- [ ] T052 [US4] `snap` cgroup root: `/sys/fs/cgroup` for exec sessions, `/proc/<target pid 1>/root/sys/fs/cgroup` for debug sessions, with a fallback note when unreadable, in src/kube/reader.sh and src/collector/kube.rs (research R7)
- [ ] T053 [US4] `ContainerUsage` with CPU % of the `cpu.max` quota and memory % of the limit, plus requests and limits from PodInfo, in src/collector/kube.rs
- [ ] T054 [US4] Container usage card replacing the battery or GPU slot in pod mode, with sources in the title, in src/ui/header.rs
- [ ] T055 [US4] Explain entries in assets/explain.toml: `container`, `pod`, `k8s.limits`, `k8s.requests`, `cgroup.cpu_throttled`, `cgroup.oom_kill`, `k8s.restarts`, `k8s.ephemeral_container`, `k8s.host_pid`, and "meminfo in a container shows the node"
- [ ] T056 [US4] Validate quickstart.md §6 on kind

**Checkpoint**: all stories work independently.

---

## Phase 7: Polish & cross-cutting

- [ ] T057 [P] README.md: Kubernetes mode section (flags, modes, what gets added to the cluster and for how long), permissions from contracts/rbac.yaml, new sources-table rows
- [ ] T058 [P] ROADMAP.md: tick the Kubernetes targets items
- [ ] T059 Measure target CPU (quickstart.md §8). If it is above 10m, apply research R2's mitigations in src/kube/reader.sh and record the result in research.md.
- [ ] T060 Run quickstart.md §7 failure table; every case shows a cause line, with no panic (SC-005)
- [ ] T061 Run `make test lint` (macOS) and `make linux-test` (Docker); all green, and snapshot changes reviewed
- [ ] T062 Check rtop idle CPU in pod mode (about 1% locally) with the existing flood harness approach

---

## Dependencies & Execution Order

### Phase dependencies

- **Setup (1)**: no dependencies.
- **Foundational (2)**: after Setup. Blocks all stories. T006 → T007 → T008 → T009 → T010 → T011
  run in sequence. T012–T014 (wire) and T015 → T016 → T017 (kube thread) can run in parallel
  with them.
- **US1 (3)**: after Foundational. It is the MVP and provides the pod handlers, Session and KubeCollector.
- **US2 (4)**: after US1's T021–T026 (it reuses the kube thread handlers, session and collector). It does not
  need T028–T033.
- **US3 (5)**: after US1 (attaches to pods). The nodes tab needs US2.
- **US4 (6)**: after US1. Independent of US2 and US3.
- **Polish (7)**: after the stories you ship.

### Within US1

T021 → T022 → T023 → T024 → T025 → T026 → T027, then T028–T033 (mostly separate files), then T034.

### Parallel opportunities

- Setup: T002, T003, T004.
- Foundational: T012–T014 and T015–T017, alongside the T006–T011 chain.
- US1 tests T018–T020 together before implementation.
- US2 tests T035–T038 together; T039 can start in parallel with US1's tail (pure function).
- US4 T049–T051 (parsers and tests) can start right after Foundational.

### Parallel example: User Story 2

```text
T035 fixtures cgroup-paths.txt + pods-on-node.json
T036 attribution tests          (after T035)
T037 helper manifest test
T038 node-mode snapshot test
```

## Implementation Strategy

### MVP first (User Story 1)

1. Phases 1–2, verifying that local snapshots are unchanged.
2. Phase 3 up to T029: exec-only pod view, which is already useful on images with a shell.
3. T030–T033: fallback, shared namespace, edge cases, actions.
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
- `ProcId` (T009) is the riskiest refactor. Land it alone, with all existing snapshots unchanged.
