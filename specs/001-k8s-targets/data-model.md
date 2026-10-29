# Data Model: Kubernetes targets

All types live in `src/model.rs` (shared) or `src/kube/` (Kubernetes-only). No persistence:
everything is held in memory for the session.

## Target

What rtop is attached to. Shown in the header (FR-008) and prefixed to provenance (R11).

| Field | Type | Notes |
|---|---|---|
| kind | `Local \| Pod \| Node` | |
| context | `String` | resolved once when the kube client is built; no other context is used |
| context_explicit | `bool` | false when it came from the kubeconfig's current context (header highlights it) |
| namespace | `Option<String>` | pod namespace; for nodes, the helper pod namespace |
| pod | `Option<String>` | |
| node | `Option<String>` | for pods: the node it runs on (from status) |
| container_filter | `Option<String>` | narrows a pod view (FR-005); changeable at runtime |

Validation:
- Exactly one of pod and node is set in Kubernetes mode.
- Pod names accept a `ns/name` shorthand.
- The context must exist in the kubeconfig; `Hello` fails with `Invalid` otherwise.

## ProcId (new key, replaces bare `Pid` as identity)

| Field | Type | Notes |
|---|---|---|
| space | `u16` | PID namespace session index; 0 for local, node, shared-namespace pods |
| pid | `Pid` | PID as seen inside that namespace |

Used by `app.rs` for:
- the tree maps
- selection
- `Scope::Family`
- details requests
- signals
- the cmdline cache (`(ProcId, starttime)`)

`ppid` resolves within the same `space`.

## Process (extended)

Existing fields unchanged, plus:

| Field | Type | Notes |
|---|---|---|
| space | `u16` | see ProcId; `Process::id()` returns `ProcId` |
| origin | `Option<Origin>` | `None` in local mode |

`Origin { pod: Option<Arc<str>>, container: Arc<str> }`:
- `container = "host"` for node processes that are not in a pod;
- `container = "pause"` for the sandbox process in shared-namespace pods.

## Session (`src/kube/session.rs`)

One long-lived reader per PID namespace.

| Field | Type | Notes |
|---|---|---|
| space | `u16` | |
| via | `Exec{container} \| Debug{container, ephemeral} \| Helper{pod}` | how it was opened; shown in provenance |
| stream | `StreamId` | exec stream on the kube thread |
| equivalent | `String` | the equivalent `kubectl exec …` command, shown in the detail view |
| state | `Probing \| Starting \| Live \| Stale{since} \| Failed{reason}` | |
| latest | `Option<(Instant, Sample)>` | last parsed `snap` |
| passwd | `HashMap<u32, String>` | from the target's `/etc/passwd` |
| env | `LinuxEnv` | target's btime, ticks, page size |

State transitions:

```
Probing ──tools ok──▶ Starting ──first frame──▶ Live ──read error / timeout──▶ Stale{since}
   │                                              ▲                              │
   └─no sh/cat─▶ (debug container) ─▶ Starting    └──────── reconnect ok ────────┘
   └─forbidden / no fallback ─▶ Failed{reason}            backoff 1,2,4…30 s
```

## Sample (one `snap` response, parsed)

| Field | Type | Notes |
|---|---|---|
| system | `stat, meminfo, loadavg, uptime, net_dev, diskstats` texts | parsed with existing `linux_parse::system` etc. |
| procs | `Vec<RemoteProc{pid, stat, uid, fds}>` | fed to `linux_parse::process_row` |
| cgroup | `Vec<(file, text)>` | container cgroup files (pod mode) |

## PodInfo / ContainerInfo (from `GetPod` and `Event::Pod`; shapes in contracts/kube-messages.md)

| Field | Source |
|---|---|
| uid, namespace, name, node, phase | `metadata.uid`, `spec.nodeName`, `status.phase` |
| share_pid_ns | `spec.shareProcessNamespace` |
| containers[].name, requests, limits | `spec.containers[].resources` |
| containers[].id, ready, restarts, state, last_termination | `status.containerStatuses[]` |
| ephemeral[] (name, running) | `status.ephemeralContainerStatuses[]`, for reuse (R3) |

## NodeIndex (node mode, pushed by a watcher)

`pod_uid → (namespace, name)` and `container_id → (pod_uid, container name)`, from
`Event::NodeIndex` (a watcher on pods with `spec.nodeName=<node>`). Joined with
`attribution::parse(cgroup_path) -> Option<(pod_uid, container_id)>`.

## ContainerUsage (Story 4)

| Field | Source |
|---|---|
| cpu_usage_usec, nr_throttled, throttled_usec | `cpu.stat` |
| memory_current, memory_max | `memory.current`, `memory.max` |
| oom_kill | `memory.events` |
| requests/limits | PodInfo |

Derived values:
- CPU % of limit, from the delta of `usage_usec` divided by the `cpu.max` quota and period;
- memory % of limit.

## HelperPod (node mode)

| Field | Notes |
|---|---|
| name | `rtop-node-<node>-<4 random>` |
| namespace | `--helper-namespace` or the current namespace |
| labels | `app.kubernetes.io/managed-by=rtop`, `rtop.io/session=<random>` |
| deadline | `activeDeadlineSeconds: 3600` |

Lifecycle: `HelperCreate` (waits for Running, 60 s, events on failure) → session →
`HelperDelete` on exit, Ctrl-C, SIGTERM or panic. `HelperGc` deletes finished helpers found by
label at the next start.
