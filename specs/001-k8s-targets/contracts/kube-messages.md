# Contract: messages between the UI side and the kube thread

All Kubernetes I/O runs on one thread with a tokio `current_thread` runtime (research R1). The
synchronous side (`collector/kube.rs`) talks to it only through two `std::sync::mpsc` channels:

```text
UI side ── Request ──▶ kube thread ── Reply / Event ──▶ UI side (try_recv only)
```

These are Rust enums in `src/kube/msg.rs`, not a wire format. This contract fixes their meaning
so both sides can be built and tested separately. Every `Request` carries an `id: u64`, and
exactly one `Reply` with that id follows.

## Requests

| Request | Reply `Ok` payload | Notes |
|---|---|---|
| `Hello { context: Option<String> }` | `Hello { context, namespace, server_version }` | builds the client; first request |
| `GetPod { ns, name }` | `PodInfo` | |
| `ListPods { ns }` | `Vec<PodSummary>` | picker |
| `ListNodes` | `Vec<NodeSummary>` | picker |
| `GetNode { name }` | `NodeInfo` (allocatable, pod count, Ready) | |
| `WatchPod { ns, name }` | `()` | then `Event::Pod(PodInfo)` on every change, and `Event::PodDeleted` |
| `WatchNodePods { node }` | `()` | then `Event::NodeIndex(NodeIndex)`, debounced 1 s |
| `Probe { ns, pod, container }` | `Probe { tools: bool, detail }` | `sh -c 'command -v cat'` |
| `EnsureDebugContainer { ns, pod, target, image, sleep_secs }` | `Debug { name, created: bool, ptrace: bool }` | reuse a running one that started after the target, or append; waits until running; `ptrace: false` when policy refused `SYS_PTRACE` and it was added without |
| `ExecOpen { ns, pod, container, command, stdin_init: Vec<u8> }` | `Stream(StreamId)` | `stdin_init` is the reader script |
| `StreamWrite { stream, data: Vec<u8> }` | `()` | |
| `StreamClose { stream }` | `()` | |
| `HelperGc { ns }` | `Vec<String>` (deleted) | finished helpers, found by label |
| `HelperCreate { node, ns, image, deadline_secs, session }` | `Helper { name }` | waits until Running (60 s) |
| `HelperDelete { ns, name }` | `()` | also sent from the exit and panic hooks |
| `Shutdown` | `()` | closes streams, deletes helpers, stops the runtime |

## Events

- `Event::Pod(PodInfo)` and `Event::PodDeleted { ns, name }`
- `Event::NodeIndex(NodeIndex)`
- `Event::StreamData { stream, data: Vec<u8> }`: stdout of the remote process. stderr goes to the
  kube log ring buffer (last 200 lines, shown in a log view).
- `Event::StreamClosed { stream, exit_code: Option<i32>, error: Option<KubeError> }`
- `Event::ThreadRestarted { reason }`: the kube thread panicked and was restarted. All sessions
  are stale until reopened.

## Errors

`Reply::Err(KubeError { code, message, verb, resource, namespace, reason })`. The `code`
values map to FR-012 messages:

| code | source |
|---|---|
| `Unreachable` | connect or TLS error |
| `Unauthorized` | HTTP 401, or an exec credential plugin failed |
| `Forbidden` | HTTP 403; `verb`, `resource` and `namespace` come from the status details |
| `NotFound` | HTTP 404 |
| `Invalid` | HTTP 422, or the pod or node is not in a usable state (`reason` filled) |
| `Admission` | 403 or 400 whose status message names an admission webhook or Pod Security |
| `Timeout` | |
| `Unsupported` | ephemeral containers disabled, or a Windows node |
| `Internal` | anything else |

## Shapes

- `PodInfo { uid, ns, name, node, phase, share_process_namespace, containers:
  Vec<ContainerInfo>, ephemeral: Vec<EphemeralInfo> }`
- `ContainerInfo { name, id, ready, restarts, state, last_termination, requests: Resources,
  limits: Resources }`
- `NodeIndex { pods: HashMap<Uid, (ns, name)>, containers: HashMap<ContainerId, (Uid, name)> }`
- `PodSummary { ns, name, phase, reason, node, containers, ready }`
- `NodeSummary { name, ready, roles, pods }`

The kube thread converts `k8s_openapi` types into these shapes with pure functions
(`src/kube/convert.rs`). Those functions are tested against JSON fixtures in
`tests/fixtures/kube/` (`pod.json`, `pod-shared.json`, `pods-on-node.json`, `node.json`).
