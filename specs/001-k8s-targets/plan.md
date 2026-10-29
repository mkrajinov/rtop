# Implementation Plan: Run rtop against Kubernetes pods and nodes

**Branch**: `001-k8s-targets` | **Date**: 2026-10-29 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/001-k8s-targets/spec.md`

## Summary

`rtop k8s --pod ns/name` and `rtop k8s --node name` run the normal TUI on the laptop. They read Linux
`/proc` data from the target through the Kubernetes API only:

- **Client**: a `src/kube/` module on **kube-rs**, in the same binary behind a default-on `kube`
  Cargo feature. All its async I/O runs on one tokio `current_thread` runtime thread, which
  talks to the synchronous UI side through channels (research R1, `contracts/kube-messages.md`).
- **Reader**: a small POSIX-`sh` reader runs in one long-lived exec session per PID namespace. It
  streams framed `/proc` text, and the laptop parses it with the existing pure `linux_parse`
  functions (R2).
- **Pods**: exec into each container first. If the image has no shell or `cat`, add a reused
  busybox ephemeral container targeting it (R3).
- **Nodes**: a labelled, self-expiring helper pod with `hostPID`. Processes are attributed to
  pods and containers from their cgroup paths, joined with a watcher-maintained index (R8, R9).
- **App changes**:
  - collection is non-blocking, and data that stops arriving is marked stale (R6);
  - process identity becomes `(space, pid)`, because containers in a pod each have their own
    PID 1 (R5).

## Technical Context

**Language/Version**: Rust stable (1.99), edition 2024

**Primary Dependencies**:
- existing: ratatui 0.30, crossterm 0.29, clap 4, procfs-core, nix, serde.
- **new, optional under the `kube` feature (default on)**:
  - `kube` (`client`, `runtime`, `ws`, `rustls-tls`);
  - `k8s-openapi` (pinned Kubernetes API version feature);
  - `tokio` (`rt`, `macros`, `time`, `io-util`, `sync`);
  - `futures`;
  - `serde_json`.
- **dev**: `tower-test` and `http`, for the mocked API server.

**Storage**: N/A (in-memory session state)

**Testing**:
- `cargo test`: unit tests and insta snapshots, fixture-driven. Covers reader frames, pod and
  node JSON, cgroup paths, and the kube thread against a mocked API service.
- `cargo test --no-default-features`: proves the local-only build still works.
- opt-in `make kube-e2e` on a local `kind` cluster.

**Target Platform**:
- rtop host: macOS and Linux.
- targets: Linux pods and nodes, cgroup v2 primary.

**Project Type**: single-binary CLI/TUI

**Performance Goals**:
- pod view < 5 s and node view < 10 s to first table (SC-001);
- ≤ 1% of one core added on the target at a 2 s tick (SC-003);
- rtop's own idle CPU stays about 1%, and the kube thread idles in epoll/kqueue.

**Constraints**:
- the UI thread never awaits or blocks on the network;
- the context is resolved once and always shown;
- read-only by default;
- cluster additions only when needed, labelled and bounded in lifetime;
- local-only build possible without tokio.

**Scale/Scope**: pods with 1–10 containers; nodes with up to about 1000 processes; one target
per run.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Checked against constitution **v1.2.0**. That amendment comes from this plan: the network clause
names rtop's Kubernetes module instead of kubectl, and heavy dependencies are allowed behind an
optional feature.

| Principle / rule | Status | Notes |
|---|---|---|
| I. Provenance | ✅ | Sources are prefixed `[pod/container]`, `[… via debug container …]` or `[node/host]`; the detail view shows the equivalent `kubectl exec` (R11). |
| II. Teach first | ✅ | New explain entries: container, limits, throttling, OOM, "meminfo shows the node", ephemeral containers, hostPID (FR-014). |
| III. Both platforms | ✅ | Kubernetes mode runs from macOS and Linux hosts; all parsing and conversion is pure and tested on every OS. |
| IV. Deterministic tests | ✅ | Fixtures plus a mocked API service; kind is opt-in. |
| V. Cheap and safe: CPU, no blocking | ✅ | Async isolated on one thread; the UI only calls `try_recv`; `Pending` details (R6). |
| V. Cheap and safe: no `fork` | ✅ | No processes are spawned at all; the kube work is a thread. |
| V. Cluster additions (v1.1.0) | ✅ | Debug container and helper pod are labelled, bounded, announced, and can be disabled (`--no-debug-container`; node mode is opt-in by flag). |
| Tech: network only via rtop's Kubernetes module, to the selected context (v1.2.0) | ✅ | One client per run, built from the resolved context. |
| Tech: dependency rule (v1.2.0) | ✅ | Heavy dependencies only under the `kube` feature; `--no-default-features` builds without them (T005 checks this). |
| Tech: UI and colour rules | ✅ | CONTAINER/POD columns and the picker use theme colours; selection is a background highlight. |

**Gate result: pass.** Post-design re-check: no new violations.

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
│   ├── kube-messages.md # Request / Reply / Event enums between UI side and kube thread
│   ├── reader-wire.md   # framed request/response protocol of the sh reader
│   ├── rbac.yaml        # minimum Role/ClusterRole per mode
│   └── helper-pod.yaml  # node helper manifest
├── checklists/requirements.md
└── tasks.md
```

### Source Code (repository root)

```text
Cargo.toml               # [features] default = ["kube"]; optional kube deps
src/
├── main.rs              # + `k8s` subcommand (--pod/--node, picker), cfg feature "kube"
├── model.rs             # + ProcId, Process.space, Process.origin, Target
├── app.rs               # Pid → ProcId keys; container filter; Pending details; stale banner
├── collector/
│   ├── mod.rs           # Collector trait: details(ProcId), target(), Pending error
│   ├── linux.rs         # unchanged behaviour; space 0
│   ├── linux_parse.rs   # reused by the remote path (+ cgroup usage parsers)
│   ├── fixture.rs       # + pod- and node-mode fixtures
│   └── kube.rs          # KubeCollector (sync): sessions, merge into Snapshot
├── kube/                # #[cfg(feature = "kube")] except the pure parts
│   ├── mod.rs           # spawn the kube thread, channels, catch_unwind + restart
│   ├── msg.rs           # Request / Reply / Event / KubeError (contracts/kube-messages.md)
│   ├── client.rs        # async handlers: hello, get/list, watchers, exec streams
│   ├── convert.rs       # pure: k8s_openapi types → PodInfo, NodeIndex, summaries
│   ├── ephemeral.rs     # probe, reuse/append debug container
│   ├── helper.rs        # helper pod manifest (pure) + lifecycle, GC, exit/panic hooks
│   ├── session.rs       # sync side of a reader session: request seq, frames → Sample, backoff
│   ├── wire.rs          # pure frame parser (always compiled, tested everywhere)
│   ├── attribution.rs   # pure cgroup path → (pod uid, container id) (always compiled)
│   └── reader.sh        # embedded POSIX sh reader
└── ui/
    ├── table.rs         # CONTAINER / POD columns when a target is set
    ├── header.rs        # target line, container usage card, stale marker
    └── picker.rs        # pod/node picker (Story 3)

tests/
├── fixtures/kube/       # *.bin reader captures, pod.json, pod-shared.json, pods-on-node.json, node.json, cgroup-paths.txt
├── kube/workloads.yaml  # kind sample workloads
└── ui.rs                # + pod/node-mode snapshots

assets/explain.toml      # + Kubernetes entries
README.md                # Kubernetes mode, permissions, sources
Makefile                 # + kube-up / kube-e2e / kube-down (context kind-rtop only); test runs --no-default-features too
```

**Structure Decision**: single crate and single binary. The Kubernetes client is one module
behind a feature, with the async code confined to `src/kube/client.rs`, `ephemeral.rs` and
`helper.rs` on its own thread. The pure parsers (`wire`, `attribution`, `convert`, the helper
manifest) have no tokio dependency and are tested on every OS.

### Delivery order (maps to user stories)

1. **Foundation**: the `ProcId` refactor (local snapshots unchanged), `Pending` details, the wire
   parser, and the feature-gated dependencies with the kube-thread skeleton.
2. **Story 1 (P1)**: pod sessions (exec), CONTAINER column, container filter; then the
   ephemeral fallback; then `shareProcessNamespace`.
3. **Story 2 (P2)**: helper pod, attribution, node index watcher, POD column.
4. **Story 4 (P4)**: container usage card and explain entries. It ranks above the picker for
   learning value.
5. **Story 3 (P3)**: picker.
6. **Polish**: README, e2e target, measurements.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Async runtime (tokio) inside a sync TUI | kube-rs is async; exec streams and watchers are long-lived sockets | Blocking HTTP would need a hand-written exec stream (WebSocket) and watch handling. A Go adapter or kubectl subprocesses add a second language or an external tool and a stringly protocol (R1). Contained to one thread and one feature. |
| Process identity `(space, pid)` instead of `pid` | Containers in a pod have separate PID namespaces, each with its own PID 1 | Renumbering PIDs would break provenance and `kill`. |
