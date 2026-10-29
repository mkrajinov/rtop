# Research: Run rtop against Kubernetes pods and nodes

All Technical Context unknowns are resolved below. Items marked **verify on kind** are
decisions whose assumption is checked by the first quickstart run; if one fails, the listed
alternative is used.

## R1. How rtop talks to the cluster

- **Decision**: spawn `kubectl` as a child process (`std::process::Command`) for every cluster
  operation: `get -o json`, `exec -i`, `debug`, `create -f -`, `delete`. Every call passes
  `--context` explicitly (resolved once at start), never relying on the current context.
- **Rationale**:
  - kubectl already handles every kubeconfig form: client certs, tokens, OIDC and
    `exec` credential plugins (`gke-gcloud-auth-plugin`, `kubelogin`, `aws eks get-token`).
  - Exec streaming (SPDY, or WebSocket on newer servers) is solved, version-negotiated and
    maintained upstream.
  - No async runtime. rtop's loop stays synchronous; background threads read child pipes.
  - It teaches: each source line can show the exact kubectl command, which the user can rerun.
  - The constitution says a new dependency needs "a reason a few lines of code cannot meet".
    Running a subprocess is a few lines; the only new crate is `serde_json`, to parse `-o json`.
  - Spawning kubectl is not a `fork` in the TUI sense (labs already re-exec through `Command`).
- **Alternatives considered**:
  - `kube` + `k8s-openapi` crates: native and typed, but pull in tokio, hyper, rustls and
    large generated types (compile time several times the current build). Exec needs an async
    WebSocket client, and auth plugins must be reimplemented or shelled out anyway.
  - Raw HTTPS to the API server: would need TLS, kubeconfig parsing, auth plugins and the
    streaming exec protocol, far beyond "a few lines".
- **Cost**: kubectl must be on PATH (or `--kubectl <path>`); rtop checks at start and says so.

## R2. Reading `/proc` in the target: the reader

- **Decision**: a single POSIX `sh` script (`src/kube/reader.sh`, embedded with `include_str!`)
  runs in a **long-lived** `kubectl exec -i … -- sh` session per PID namespace. rtop writes
  requests to its stdin (`snap`, `cmd <pids>`, `details <pid>`, `kill <sig> <pid>`) and reads
  framed responses from stdout (see `contracts/reader-wire.md`).
- **Rationale**:
  - One exec per session instead of one per tick or file: no repeated API-server upgrade and
    kubelet/runtime exec setup (each costs about 100–300 ms and an audit log entry).
  - Needs only `sh` + `cat`; most per-process work uses builtins:
    - fd count: `set -- /proc/$p/fd/*; echo $#`;
    - UID: `while read` over `status` up to the `Uid:` line, so only that line is sent;
    - `stat` lines: read with `read -r`.
  - Parsing stays on the laptop in the existing pure `linux_parse` functions, which already
    run and are tested on macOS. The remote side only moves bytes.
- **Bytes per tick**: about 300 bytes per process (stat line, uid, fd count) and command lines
  only for new PIDs (requested with `cmd`). That is about 150 KB per tick for a 500-process node
  and a few KB for a pod.
- **CPU on target** (SC-003, **verify on kind**): target ≤ 1% of one core at a 2 s tick.
  - Mitigations in order: drop the fd count from `snap` (fetch on details only); raise the tick.
  - Last resort: a static `rtop __reader` binary in the debug image (see R10).
- **Alternatives considered**:
  - One `kubectl exec cat …` per file: hundreds of execs per tick.
  - `kubectl cp` of an rtop binary into the target: writes into the app container, needs `tar`,
    and breaks "add nothing" (FR-003).
  - A metrics API: no per-process data.

## R3. Exec first, ephemeral fallback (FR-003)

- **Decision**:
  - **Probe**: `kubectl exec <pod> -c <c> -- sh -c 'command -v cat'`, one call per container.
  - **Success**: start the reader session in that container.
  - **Failure** ("executable file not found", or exit 127): unless `--no-debug-container` is
    set, run
    `kubectl debug <pod> -c rtop-<c>-<4 random> --target=<c> --image=<debug-image> --profile=general -- sleep 3600`.
    Then exec the reader into that ephemeral container.
  - **Reuse**: an ephemeral container named `rtop-<c>-*` that is still running is reused, never
    duplicated.
  - **Notice**: rtop tells the user a debug container was added and that it stays in the pod
    spec until the pod is replaced.
- **Rationale**:
  - `--target` puts the debug container in the target container's PID namespace (requires
    container runtime support; containerd and CRI-O have it).
  - `--profile=general` adds `SYS_PTRACE`, which `ptrace_may_access` needs to read
    `environ`, `fd/`, `maps` and `root/` of processes owned by other UIDs.
  - `sleep 3600` bounds how long the idle container lives. Ephemeral containers cannot be
    removed, only left to exit.
- **Debug image**: default `busybox:1.37` (about 4 MB, has `sh`, `cat`, `kill` and `renice`),
  overridable with `--debug-image` for registries behind a mirror. No rtop image needs
  publishing.
- **Alternatives considered**:
  - Always ephemeral: works everywhere, but leaves a trace in every pod inspected.
  - Exec only: fails on distroless images.

## R4. Pod with several containers: one session per PID namespace

- **Decision**:
  - **Default** (`shareProcessNamespace` false): one reader session per running container. Each
    session's processes get that container's name.
  - **`shareProcessNamespace: true`**: all containers share one PID namespace, so one session is
    enough and PIDs are unique.
    - Container attribution: match each process's `/proc/<pid>/cgroup` path against the
      container IDs in `status.containerStatuses[].containerID`.
    - The pause process is labelled `pause`.
- **Init and finished containers**: not read. Their state comes from pod status (spec edge case).

## R5. Process identity (two PID 1s)

- **Decision**: introduce `ProcId { space: u16, pid: Pid }`. `space` is the index of the PID
  namespace session (0 for local, node, and shared-namespace pods). Everything in `app.rs` that
  keys on `Pid` keys on `ProcId` instead:
  - the tree maps (`by_pid`, `children`)
  - `selected_pid`
  - `Scope::Family`
  - `details()`
  - signals
  - the cmdline cache

  `ppid` stays a plain `Pid` resolved inside the same `space`.
- **Rationale**: parent links never cross a PID namespace from the reader's point of view. One
  small key type keeps the change mechanical. Local mode uses `space = 0` everywhere, so
  snapshots do not change.

## R6. UI must not block: background collection

- **Decision**: `KubeCollector` owns a worker thread per reader session plus one for pod and
  node status. Each worker keeps the latest parsed result behind a `Mutex`.
  `Collector::snapshot()` merges the latest results and returns at once.
  - **Stale data**: a result older than 2 ticks is marked stale with its age (FR-012). Workers
    restart their session with backoff (1, 2, 4 … 30 s).
  - **Details**: `details(id)` returns a typed `Pending` error on first call and starts a fetch.
    The app already re-requests details on every refresh, so the panel shows "loading…" and
    fills in on the next tick.
- **Rationale**: Principle V (collection must not block the UI). Only one new error variant
  is needed; the app loop is unchanged.

## R7. Container resource usage (Story 4)

- **Requests and limits**: from the pod spec (`kubectl get pod -o json`), provenance
  `pod.spec.containers[name].resources`.
- **Restarts, last termination reason, OOMKilled**: from `status.containerStatuses`.
- **Usage**: cgroup v2 files `cpu.stat` (`usage_usec`, `nr_throttled`, `throttled_usec`),
  `memory.current`, `memory.max` and `memory.events` (`oom_kill`):
  - **exec session**: `/sys/fs/cgroup/<file>`. With a private cgroup namespace, the
    container's own cgroup is the root.
  - **ephemeral session**: `/proc/<target pid 1>/root/sys/fs/cgroup/<file>` (needs `SYS_PTRACE`,
    which the general profile grants).
  - **verify on kind**: if `root/` is unreadable, fall back to the spec limits and
    `status`, and mark usage "not readable from debug container".
- **cgroup v1**: show what maps (`memory.usage_in_bytes`, `cpu.stat`), otherwise "n/a (cgroup
  v1)".

## R8. Node mode: helper pod

- **Decision**: rtop creates the pod itself with `kubectl create -f -`, rather than through
  `kubectl debug node/…`, so it controls labels, lifetime and mounts. Manifest
  (`contracts/helper-pod.yaml`):
  - **Scheduling**: `nodeName: <node>`, tolerations `operator: Exists`.
  - **Host access**: `hostPID: true`, and `hostPath /sys/fs/cgroup` mounted read-only at
    `/host/sys/fs/cgroup`.
  - **Security**: `securityContext.capabilities.add: [SYS_PTRACE]` and `runAsUser: 0`. Not
    privileged.
  - **Lifetime**: `activeDeadlineSeconds: 3600` and `restartPolicy: Never`.
  - **Labels**: `app.kubernetes.io/managed-by: rtop` and `rtop.io/session: <random>`.
  - **Image**: the same debug image as R3; command `sleep 3600`.
- **Cleanup**:
  - On normal exit, Ctrl-C or SIGTERM: rtop deletes the pod (`--wait=false`).
  - On a hard kill: the kubelet stops the pod at the deadline.
  - At the next start: rtop deletes its own finished helpers, found by label, in that namespace.
- **Pod Security admission**: `hostPID` and `hostPath` need the `privileged` level. If
  creation is rejected, rtop shows the admission message and suggests
  `--helper-namespace <ns>` (SC-004, FR-012).
- **Alternatives considered**: `kubectl debug node/X` gives no control over deadline or labels,
  and its profiles are either too weak (no host cgroup) or `sysadmin` (fully privileged).

## R9. Node mode: attributing processes to pods and containers

- **Decision**:
  - Read `/proc/<pid>/cgroup` for each new `(pid, starttime)` and cache it. Map it with a pure
    function in `src/kube/attribution.rs`:
    - `pod<UID>` with `_` → `-` (systemd driver: `kubepods-burstable-pod1234_5678.slice`) or
      `pod<UID>` directly (cgroupfs driver) gives the pod;
    - `cri-containerd-<id>.scope`, `crio-<id>.scope`, `docker-<id>.scope` or a bare 64-hex
      segment gives the container ID.
  - Join with `kubectl get pods -A --field-selector spec.nodeName=<node> -o json`, refreshed every
    10 s or when an unknown ID appears. That gives pod UID → namespace/name, and container ID
    → name.
  - Processes with no match are `host`.
- **Cgroup namespace (verify on kind)**: the helper has its own cgroup namespace, so paths
  outside it appear as `/../../kubepods.slice/...`. The matcher searches segments anywhere in
  the path, so the prefix doesn't matter. Fixtures cover both drivers and the `/..` form.

## R10. Agent binary (deferred)

A static `rtop __reader` (musl) in an `rtop-debug` image would cut target CPU and bytes further.
This is deferred until R2's measurement shows `sh` is too slow. The wire contract is designed so
a binary reader can speak it unchanged.

## R11. Remote identity data

- **User names**: read `/etc/passwd` once per session (`cat /etc/passwd`). If it is missing
  (distroless), show numeric UIDs. In node mode the helper's `/etc/passwd` is wrong, so read
  `/proc/1/root/etc/passwd` (host init's root).
- **`LinuxEnv`**:
  - `btime` from the target's `/proc/stat`;
  - `ticks` = 100 (`USER_HZ` is fixed at 100 in the Linux user ABI on x86 and arm64);
  - `page_size` from `getconf PAGESIZE` if present, otherwise derived from
    `statm`/`status` VmRSS once per session.
- **Provenance**: every `Field.source` and raw view title is prefixed with where it was read:
  - `[pod/container]` for exec;
  - `[pod/container via debug container rtop-…]` for the fallback;
  - `[node/host]` for node mode.

  The detail view shows the kubectl command that opened the session.

## R12. Test environment and safety

- `kind` 0.x and `minikube` are installed locally; quickstart uses `kind` (Docker is running).
- All manual and automated cluster steps pass `--context kind-rtop` explicitly. No command is
  run against other contexts (the current context is a production cluster).
- Unit and snapshot tests use recorded fixtures under `tests/fixtures/kube/`: reader output
  frames, `get pod -o json`, `get pods -A -o json` and cgroup paths. They need no cluster.
- A `make kube-e2e` target (opt-in, not part of `make test`) creates the kind cluster, deploys
  sample workloads (multi-container, distroless, shareProcessNamespace) and runs the quickstart
  checks.
