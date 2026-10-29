# Research: Run rtop against Kubernetes pods and nodes

All Technical Context unknowns are resolved below. Items marked **verify on kind** are
decisions whose assumption is checked by the first quickstart run; if one fails, the listed
alternative is used.

## R1. How rtop talks to the cluster: kube-rs in the same binary

- **Decision**: a `src/kube/` module built on **kube-rs** (`kube` with the `client`,
  `runtime`, `ws` and `rustls-tls` features, plus `k8s-openapi`), compiled in behind a Cargo
  feature `kube` that is on by default.
- **Threading**: all Kubernetes I/O runs on one dedicated thread with a tokio `current_thread`
  runtime. The synchronous rest of rtop talks to it only through channels: `Request` in,
  `Reply`/`Event` out (`contracts/kube-messages.md`).
- **What the module does**:
  - **Kubeconfig**: `Config::from_kubeconfig(&KubeConfigOptions { context, .. })` with the
    same loading rules as kubectl, including `exec` credential plugins (`gke-gcloud-auth-plugin`,
    `kubelogin`, `aws eks get-token`), OIDC, certificates and tokens.
  - **Exec**: `Api::<Pod>::exec` over WebSocket (`ws` feature). It returns an attached process
    with stdin and stdout handles and the final status.
  - **Watches**: `kube::runtime::watcher` for the target pod (restart and delete events arrive
    immediately) and for pods on a node (the node index). There is no polling.
  - **Writes**: the `ephemeralcontainers` subresource for the debug container, and
    `Api::<Pod>::create`/`delete` for the helper pod.
  - **Errors**: `kube::Error::Api(ErrorResponse { code, reason, .. })`, mapped to typed
    `KubeError` codes. No text parsing.
- **Rationale**:
  - One language, one toolchain and one binary. `cargo install` keeps working, with no
    cross-process protocol to version.
  - Rust types are shared directly. The message enums replace a wire protocol and its contract
    tests.
  - The async code is confined to one thread and module, so `app.rs`, `ui/` and the local
    collectors stay synchronous. Principle V's "never block the UI" holds: the UI thread only
    calls `try_recv`.
  - Behind the `kube` feature, `cargo build --no-default-features` stays a lean local-only build
    without tokio, hyper or k8s-openapi. That is the constitution's dependency justification.
- **Alternatives considered**:
  - **Go adapter on client-go** (separate `rtop-kube` binary over a JSON stdio protocol): the
    reference client, with clean crash isolation. Rejected for the cost of a second language
    and toolchain, two binaries, and a versioned protocol with base64 streams.
  - **`kubectl` subprocesses**: very few lines, but stringly typed (stderr parsing), polling
    instead of watches, one process per call, and an external tool dependency.
- **Costs**: a slower clean build and a binary several MB larger with the feature on. Both are
  measured in T005, and the default can be revisited.
  - **Measured (T005, 2026-10-29, after the pod view landed)**: on an Apple-silicon Mac, a
    clean `cargo build --release` (thin LTO, stripped) takes 65 s and makes a 10.8 MB binary.
    With `--no-default-features` it takes 19 s and the binary is 1.95 MB. So the feature costs
    about 46 s of clean build time and about 8.8 MB. Incremental builds are unaffected. Kept on
    by default: Kubernetes mode is a headline feature, and the lean build is one flag away.
- **Crash isolation**: the kube thread runs inside `catch_unwind`. A panic there marks every
  session stale and restarts the thread with backoff. It never takes down the UI.

## R2. Reading `/proc` in the target: the reader

- **Decision**: a single POSIX `sh` script (`src/kube/reader.sh`, embedded with `include_str!`).
  - It is written as the first stdin input of one long-lived exec session (`sh`) per PID
    namespace.
  - rtop then sends requests on that stdin (`snap`, `cmd <pids>`, `details <pid>`,
    `kill <sig> <pid>`) and reads framed responses from stdout (`contracts/reader-wire.md`).
- **Rationale**:
  - One exec per session, not per tick or file. That avoids repeating the API-server upgrade and
    the kubelet and runtime exec setup (100–300 ms and an audit-log entry each).
  - It needs only `sh` and `cat`. Most per-process work uses shell builtins:
    - fd count: `set -- /proc/$p/fd/*; echo $#`;
    - UID: a `while read` loop over `status`, so only the `Uid:` line is sent;
    - stat lines: `read -r`.
  - Parsing stays in rtop's pure `linux_parse` functions, which already run and are tested on
    macOS.
- **Bytes per tick**: about 300 bytes per process (stat line, uid, fd count). Command lines are
  requested only for new PIDs. That is about 150 KB per tick for a 500-process node.
- **CPU on target** (SC-003, **verify on kind**): target ≤ 1% of one core at a 2 s tick.
  - Mitigations, in order: drop the fd count from `snap`; raise the tick.
  - Last resort: a static reader binary (R10).
- **Alternatives considered**:
  - One exec per file: hundreds of execs per tick.
  - Copying a binary into the app container: writes into the workload and needs `tar`.
  - The metrics API: no per-process data.

## R3. Exec first, ephemeral fallback (FR-003)

- **Decision**:
  1. **Probe**: exec `sh -c 'command -v cat'` in the container. Exit 0 means the tools are
     there. Exit 127, or a runtime error saying the executable was not found, means they are
     missing. Other errors are reported as errors.
  2. **Tools present**: open the reader session in that container.
  3. **Tools missing, and `--no-debug-container` not set**:
     - Reuse a running ephemeral container named `rtop-<c>-*`.
     - Otherwise patch the pod's `ephemeralcontainers` subresource to append:
       - `name: rtop-<c>-<4 random>`, `targetContainerName: <c>`, `image: <debug-image>`;
       - `command: [sleep, "3600"]`;
       - `securityContext.capabilities.add: [SYS_PTRACE]` (what `kubectl debug
         --profile=general` sets).
     - Wait until it runs, via the pod watcher, with a 60 s timeout.
  4. Open the reader session in the debug container.
  5. Tell the user a debug container was added, and that it stays in the pod spec until the pod
     is replaced.
- **Rationale**:
  - `targetContainerName` puts the debug container in the target's PID namespace (containerd
    and CRI-O support it).
  - `SYS_PTRACE` is what `ptrace_may_access` requires to read `environ`, `fd/`, `maps` and
    `root/` of processes owned by other UIDs.
  - `sleep 3600` bounds the idle container's life. Ephemeral containers cannot be removed, only
    left to exit.
- **Debug image**: default `busybox:1.37` (about 4 MB; has `sh`, `cat`, `kill`, `renice`).
  Override with `--debug-image`. No rtop image needs publishing.
- **Alternatives considered**:
  - Always using ephemeral containers: leaves a trace in every pod inspected.
  - Exec only: fails on distroless images.

## R4. Pod with several containers: one session per PID namespace

- **Default** (`shareProcessNamespace: false`): one reader session per running container. Its
  processes get that container's name.
- **`shareProcessNamespace: true`**: one session; PIDs are unique.
  - Container attribution matches each process's `/proc/<pid>/cgroup` path against
    `containerStatuses[].containerID` from the pod watcher.
  - The pause process is labelled `pause`.
- **Init and finished containers**: not read. Their state comes from pod status.

## R5. Process identity (two PID 1s)

- **Decision**: introduce `ProcId { space: u16, pid: Pid }`. `space` is the index of the PID
  namespace session: 0 for local, node and shared-namespace pods.
- **Re-keyed in `app.rs` from `Pid` to `ProcId`**:
  - the tree maps (`by_pid`, `children`);
  - `selected_pid`;
  - `Scope::Family`;
  - `details()`;
  - signals;
  - the cmdline cache.
- **`ppid`** stays a plain `Pid`, resolved within the same `space`.
- **Rationale**: parent links never cross a PID namespace from the reader's point of view. Local
  mode uses `space = 0`, so snapshots do not change.

## R6. UI must not block: background collection

- **Collection**:
  - The kube thread owns every exec stream and watcher. For each reader session it keeps the
    latest parsed sample in an `Arc<Mutex<..>>`.
  - `Collector::snapshot()` (UI thread) merges the latest samples and drains the event channel
    with `try_recv`. It never awaits.
- **Stale data**:
  - A sample older than 2 ticks is marked stale with its age (FR-012).
  - Sessions reopen with backoff (1, 2, 4 … 30 s).
- **Details**: `details(id)` returns a typed `Pending` error on the first call and sends a
  `Details` request. The app already re-requests details on every refresh, so the panel shows
  "loading…" until the reply arrives.

## R7. Container resource usage (Story 4)

- **Requests and limits**: from the pod object (watcher), provenance
  `pod.spec.containers[name].resources`.
- **Restarts, last termination reason, OOMKilled**: from `status.containerStatuses`.
- **Usage**: cgroup v2 files `cpu.stat` (`usage_usec`, `nr_throttled`, `throttled_usec`),
  `memory.current`, `memory.max` and `memory.events` (`oom_kill`). Where they are read:
  - **exec session**: `/sys/fs/cgroup/<file>` (with a private cgroup namespace, the
    container's own cgroup is the root);
  - **debug session**: `/proc/<target pid 1>/root/sys/fs/cgroup/<file>` (needs `SYS_PTRACE`);
  - **verify on kind**: if `root/` is unreadable, fall back to the spec and status values and
    show usage as "not readable from debug container".
- **cgroup v1**: show what maps; otherwise "n/a (cgroup v1)".

## R8. Node mode: helper pod

- **Creation**: rtop creates the pod from the manifest in `contracts/helper-pod.yaml`, built as
  a typed `k8s_openapi::api::core::v1::Pod`:
  - **Scheduling**: `nodeName`, tolerations `Exists`.
  - **Host access**: `hostPID`; read-only `hostPath /sys/fs/cgroup`.
  - **Security**: `SYS_PTRACE`, `runAsUser: 0`. Not privileged.
  - **Lifetime**: `activeDeadlineSeconds: 3600`, `restartPolicy: Never`.
  - **Labels**: `app.kubernetes.io/managed-by: rtop`, `rtop.io/session: <random>`.
  - **Image**: the debug image, running `sleep 3600`.
  - **Startup**: waits for Running via a watcher (60 s). On failure it lists the pod's events
    and the admission rejection message.
- **Cleanup**:
  - rtop deletes the helper on normal exit, Ctrl-C and SIGTERM, and from a panic hook.
  - The pod deadline covers a hard kill.
  - At start, finished rtop helpers are deleted by label.
- **Pod Security admission**: `hostPID` and `hostPath` need `privileged`. The admission
  message is shown with a `--helper-namespace` hint.
- **Alternative considered**: the same thing as `kubectl debug node/X`, which offers no control
  of deadline or labels.

## R9. Node mode: attributing processes to pods and containers

- **Matching**: a pure `attribution::parse(cgroup_path)` that recognises:
  - `pod<UID>`: the systemd driver writes `_` for `-` in the UID; the cgroupfs driver writes the
    UID unchanged;
  - container IDs in `cri-containerd-<id>.scope`, `crio-<id>.scope`, `docker-<id>.scope`, or a
    bare 64-hex segment.

  It matches segments anywhere in the path, so the `/../..` prefix seen from inside the
  helper's cgroup namespace doesn't matter (**verify on kind**).
- **Node index**: a `watcher` with field selector `spec.nodeName=<node>` maintains pod UID →
  namespace/name and container ID → (pod UID, container name). It sends a `NodeIndex` event
  when the index changes, debounced to 1 s.
- **Result**: processes with no match are labelled `host`.

## R10. Reader binary (deferred)

A static reader binary (Rust musl, or a small Go program as a side project) in an `rtop-debug`
image would cut target CPU and bytes further. Deferred until R2's measurement shows `sh` is too
slow. The wire contract is designed so a binary reader can speak it unchanged.

## R11. Remote identity data

- **User names**: read `/etc/passwd` once per session; numeric UIDs if it is missing. In node
  mode, read `/proc/1/root/etc/passwd` (host init's root).
- **`LinuxEnv`**:
  - `btime` from the target's `/proc/stat`;
  - `ticks` = 100 (`USER_HZ` is fixed in the Linux user ABI);
  - page size from `getconf PAGESIZE`, or derived once per session.
- **Provenance**: every source is prefixed with where it was read:
  - `[pod/container]` for exec;
  - `[pod/container via debug container rtop-…]` for the fallback;
  - `[node/host]` for node mode.

  The detail view also shows the equivalent `kubectl exec …` command, so the user can
  reproduce the read by hand.

## R12. Test environment and safety

- **Clusters**: `kind` and `minikube` are installed; quickstart uses `kind` (Docker is
  running).
- **Context safety**: all manual and e2e steps pass `--context kind-rtop` explicitly. The
  current context is a production cluster, so:
  - the header always shows the context;
  - a context that was not given explicitly is shown highlighted.
- **Unit and snapshot tests**: fixtures under `tests/fixtures/kube/`, covering reader frames,
  pod and node objects as JSON, and cgroup paths.
- **kube-thread logic**: the ephemeral reuse, helper manifest, GC by label and error mapping are
  tested against a mocked HTTP service (`tower_test::mock` with `kube::Client::new`), so no
  cluster is needed.
- **E2E**: `make kube-e2e` (opt-in) creates the kind cluster, deploys sample workloads and runs
  the quickstart checks.
