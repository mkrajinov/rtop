# Implementation Plan: Run rtop against Kubernetes pods and nodes

**Branch**: `001-k8s-targets` | **Date**: 2026-10-29 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/001-k8s-targets/spec.md`

## Summary

`rtop --pod ns/name` and `rtop --node name` run the normal TUI on the laptop. They read Linux
`/proc` data from the target through the Kubernetes API only:

- **Transport**: `kubectl` child processes inherit every kubeconfig auth form (research R1).
- **Reader**: a small POSIX-`sh` reader runs in one long-lived `kubectl exec -i` session per PID
  namespace. It streams framed `/proc` text, and the laptop parses it with the existing pure
  `linux_parse` functions (R2).
- **Pods**: exec into each container first. If the image has no shell or `cat`, add a reused
  busybox ephemeral container targeting it (R3).
- **Nodes**: a labelled, self-expiring helper pod with `hostPID` on the node (R8). Processes are
  attributed to pod and container from their cgroup path (R9).
- **App changes**:
  - collection moves to background threads, so the UI never waits on the network (R6);
  - process identity becomes `(space, pid)`, because containers in a pod each have their own
    PID 1 (R5).

## Technical Context

**Language/Version**: Rust stable (1.99), edition 2024

**Primary Dependencies**:
- existing: ratatui 0.30, crossterm 0.29, clap 4, procfs-core, nix, serde;
- **new**: `serde_json` (parse `kubectl -o json`);
- **external tool**: `kubectl` on PATH.

**Storage**: N/A (in-memory session state)

**Testing**:
- `cargo test`: unit tests plus insta snapshots from fixtures, with recorded reader output and
  kubectl JSON under `tests/fixtures/kube/`;
- opt-in `make kube-e2e` on a local `kind` cluster.

**Target Platform**:
- rtop host: macOS and Linux;
- targets: Linux pods and nodes, cgroup v2 primary.

**Project Type**: single-binary CLI/TUI

**Performance Goals**:
- pod view < 5 s and node view < 10 s to first table (SC-001);
- ≤ 1% of one core added on the target at a 2 s tick (SC-003);
- rtop's own idle CPU stays about 1%.

**Constraints**:
- UI thread never blocks on kubectl;
- only `--context` passed explicitly;
- read-only by default;
- nothing added to pods unless the fallback or node mode needs it, and then labelled and
  bounded in lifetime.

**Scale/Scope**: pods with 1–10 containers; nodes with up to about 1000 processes; one target
per run.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle / rule | Status | Notes |
|---|---|---|
| I. Provenance | ✅ | Each source is prefixed `[pod/container]`, `[… via debug container …]` or `[node/host]`, and the detail view shows the kubectl command (R11). |
| II. Teach first | ✅ | New explain entries: container column, cgroup limits, throttling, OOM, "meminfo shows the node", ephemeral containers, hostPID (FR-014). |
| III. Both platforms | ✅ | Kubernetes mode runs from macOS and Linux hosts. All parsing (wire, attribution, kubectl JSON) is pure and tested on every OS. |
| IV. Deterministic tests | ✅ | Fixtures under `tests/fixtures/kube/`; the fixture collector gains a pod target for snapshots; the cluster test is opt-in. |
| V. Cheap and safe: CPU, no blocking | ✅ | Background workers, latest-value cache, `Pending` details (R6). |
| V. Cheap and safe: no `fork` | ✅ | kubectl runs through `Command` (spawn and exec), like the lab re-exec. |
| V. "read-only except signal, renice, labs" | ✅ (v1.1.0) | Node mode creates and deletes a helper pod; the fallback patches `pods/ephemeralcontainers`. |
| Tech: "No network access at runtime" | ✅ (v1.1.0) | Kubernetes mode talks to the API server, which is the feature. |
| Tech: minimal dependencies | ✅ | Only `serde_json`; kube-rs rejected (R1). |
| Tech: UI and colour rules | ✅ | New CONTAINER/POD columns and picker use theme colours; selection is a background highlight, no edge stripes. |

**Gate result: pass after constitution amendment v1.1.0 (applied 2026-10-29).** Both conflicts are
permanent properties of this feature, not one-off exceptions, so the right fix is a MINOR
amendment (v1.0.0 → v1.1.0) via `/speckit-constitution`:

- Technical Constraints: "No network access at runtime" becomes:
  > No network access at runtime, except Kubernetes mode, which talks only to the API server of
  > the kubeconfig context the user selected, through `kubectl`. No telemetry.
- Principle V: "rtop is read-only except for explicit user actions (signal, renice) and labs"
  becomes:
  > … and labs. In Kubernetes mode, rtop may add only what reading requires (an ephemeral debug
  > container, a node helper pod). Each addition is labelled as rtop's, has a bounded lifetime,
  > is announced to the user, and can be disabled.

Post-design re-check (after Phase 1): no new violations. The design stays inside the amended
wording: helper pod with label and `activeDeadlineSeconds`, debug container with `sleep 3600`
and a notice, and `--no-debug-container`.

## Project Structure

### Documentation (this feature)

```text
specs/001-k8s-targets/
├── spec.md
├── plan.md              # this file
├── research.md          # R1–R12 decisions
├── data-model.md        # Target, ProcId, Session, Sample, PodInfo, NodeIndex, HelperPod
├── quickstart.md        # kind-based end-to-end validation
├── contracts/
│   ├── cli.md           # flags, defaults, exit messages
│   ├── reader-wire.md   # framed request/response protocol of the sh reader
│   ├── rbac.yaml        # minimum Role/ClusterRole per mode
│   └── helper-pod.yaml  # node helper manifest
├── checklists/requirements.md
└── tasks.md             # /speckit-tasks (not yet created)
```

### Source Code (repository root)

```text
src/
├── main.rs              # + --pod/--node/--kube flags, mode selection (contracts/cli.md)
├── model.rs             # + ProcId, Process.space, Process.origin, Target
├── app.rs               # Pid → ProcId keys; container filter; Pending details; stale banner
├── collector/
│   ├── mod.rs           # Collector trait: details(ProcId), target(), Pending error
│   ├── linux.rs         # unchanged behaviour; uses ProcId{space:0}
│   ├── linux_parse.rs   # reused as-is by the remote path (+ cgroup usage parsers)
│   ├── fixture.rs       # + a pod-mode fixture (two containers, two PID 1s)
│   └── kube.rs          # KubeCollector: merges session samples into Snapshot
├── kube/
│   ├── mod.rs           # Target resolution, context check
│   ├── kubectl.rs       # spawn helpers, JSON → PodInfo/NodeIndex (serde_json)
│   ├── session.rs       # long-lived exec session + worker thread, backoff, state machine
│   ├── reader.sh        # embedded POSIX sh reader (include_str!)
│   ├── wire.rs          # pure frame parser → Sample / detail files
│   ├── attribution.rs   # pure cgroup path → (pod uid, container id)
│   ├── ephemeral.rs     # probe, create/reuse debug container
│   └── helper.rs        # node helper pod lifecycle and cleanup
└── ui/
    ├── table.rs         # CONTAINER / POD columns when target is Kubernetes
    ├── header.rs        # target line, container usage vs limits card, stale marker
    └── picker.rs        # pod/node picker (Story 3)

tests/
├── fixtures/kube/       # hello.bin, snap-pod.bin, snap-node.bin, details.bin, pod.json, pods-on-node.json, cgroup-paths.txt
├── kube/workloads.yaml  # kind sample workloads for quickstart / make kube-e2e
└── ui.rs                # + pod-mode snapshots (container column, two PID 1s, stale banner)

assets/explain.toml      # + container, pod, origin, cgroup usage, throttling, oom, ephemeral, hostPID entries
README.md                # Kubernetes mode section, permissions (contracts/rbac.yaml), sources table rows
Makefile                 # + kube-up / kube-e2e / kube-down (kind, context kind-rtop only)
```

**Structure Decision**: single crate. Kubernetes code is a new `src/kube/` module plus one
collector (`collector/kube.rs`) implementing the existing `Collector` trait, so the UI keeps a
single data path. It is not cfg-gated: it runs on macOS and Linux hosts.

### Delivery order (maps to user stories)

1. **Foundation**:
   - `ProcId` refactor across `app.rs` (local behaviour and snapshots unchanged);
   - the `Pending` details error;
   - the wire parser and fixtures.
2. **Story 1 (P1)**: kubectl wrapper, exec session and reader, KubeCollector for pods
   (one session per container, CONTAINER column, container filter); then the ephemeral
   fallback; then `shareProcessNamespace` attribution.
3. **Story 2 (P2)**: helper pod lifecycle, attribution, NodeIndex, POD column.
4. **Story 3 (P3)**: picker.
5. **Story 4 (P4)**: container usage vs limits card, explain entries.
6. **Polish**: README permissions and sources, `make kube-e2e`, `--allow-actions` with
   confirmation prompts.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Network access at runtime (Kubernetes mode) | The feature is reading remote pods and nodes through the API server | None exists: the spec forbids SSH, and local-only cannot see a cluster. Resolved by the proposed amendment, not by exception. |
| Writes to the cluster (ephemeral container, helper pod) | Distroless images have no reader tools; nodes have no exec endpoint without a pod | Exec-only fails on distroless (spec Q1 chose C). `kubectl debug node` gives no label or deadline control. Writes are labelled, bounded and can be disabled. |
| Process identity `(space, pid)` instead of `pid` | Containers in one pod have separate PID namespaces, each with its own PID 1 | Renumbering PIDs would break provenance (the PID shown must be the real one in its namespace) and `kill`. |
