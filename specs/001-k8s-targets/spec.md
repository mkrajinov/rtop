# Feature Specification: Run rtop against Kubernetes pods and nodes

**Feature Branch**: `001-k8s-targets`

**Created**: 2026-10-29

**Status**: Draft

**Input**: User description: "option to start rtop against pod in k8s over kube api"; refined:
"we need it for nodes too"; "we might have pod and list processes of all containers, just give
container column"; reaching the container: "C option" (exec first, ephemeral debug container as
fallback).

## Clarifications

### Session 2026-10-29

- Q: What may rtop leave in or require from the pod? → A: Try exec with tools already in the
  image; when the image lacks them (distroless, scratch), fall back to an ephemeral debug
  container that carries rtop's reader.
- Q: Is the target a pod or a container? → A: A pod; the table lists processes of all its
  containers with a container column. A single container can be selected to narrow the view.
- Q: Pods only? → A: Nodes too: all processes on a node, each labelled with the pod and container
  it belongs to, or as a host process.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Inspect all processes of a pod (Priority: P1)

An engineer starts rtop with a pod as the target (for example `rtop k8s --pod my-ns/api-7c9f`, using
their current kubeconfig context). rtop connects through the Kubernetes API, without SSH to the
node, and shows the processes of **every container in the pod** in one table with a CONTAINER
column. Sorting, filtering, tree view, detail tabs, explain panel and raw viewer work as in a
local run, and every value names the `/proc` file it came from inside its container.

**Why this priority**: seeing what actually runs in a pod (each container's PID 1, sidecars,
threads, open files, limits, namespaces, cgroup) through the normal cluster access path is the
core value, and it teaches that a container is just processes with namespaces and a cgroup.

**Independent Test**: on a local cluster (kind/minikube), start rtop against a pod with an app
container and a sidecar; the table lists exactly the union of `kubectl exec <pod> -c <c> -- ps -ef`
for each container, each row labelled with its container.

**Acceptance Scenarios**:

1. **Given** a running pod with two containers and a kubeconfig with access, **When** the user
   starts rtop with that pod as target, **Then** within 5 seconds the table shows the processes
   of both containers, the CONTAINER column names each row's container, and the header names
   the context, namespace, pod and node.
2. **Given** containers that each have their own PID namespace, **When** both have a PID 1,
   **Then** both rows are shown, distinguished by container, and actions and detail views apply to
   the right one.
3. **Given** a pod whose image has no shell or `cat` (distroless), **When** rtop attaches, **Then**
   it still shows that container's processes (via the fallback) and tells the user that a debug
   container was added to the pod.
4. **Given** rtop is attached to a pod, **When** the user narrows the view to one container
   (start option or in-app filter), **Then** only that container's processes are shown, and the
   choice can be changed without restarting rtop.
5. **Given** rtop is attached, **When** a process starts or exits in any container, **Then** the
   table reflects it within two refresh ticks.

---

### User Story 2 - Inspect all processes of a node (Priority: P2)

An engineer starts rtop with a node as the target (for example `rtop --node worker-3`). rtop
shows every process on that node: kubelet, container runtime and other host processes, plus all
containers' processes. Each row's POD and CONTAINER columns show which pod and container it
belongs to, or `host`. The dashboard shows the node's CPU, memory, disk and network, as it does
locally.

**Why this priority**: node-level debugging (a noisy neighbour, kubelet or runtime trouble, memory
pressure) is the other half of the SRE use case, and the node view shows how pods map onto plain
processes and cgroups on the host. It builds on the reader from Story 1.

**Independent Test**: on a local cluster, start rtop against a node; the process count matches
`ps -e` on the node (seen through `kubectl debug node/...`), and processes of a known pod are
labelled with that pod and container.

**Acceptance Scenarios**:

1. **Given** a Ready node and permission to run a debug pod there, **When** the user starts rtop
   with that node as target, **Then** within 10 seconds the table shows all node processes, with
   POD/CONTAINER columns filled for container processes and `host` for the rest.
2. **Given** rtop is attached to a node, **When** the user filters by a pod name, **Then** only
   that pod's processes are shown.
3. **Given** rtop is attached to a node, **When** the user quits rtop (normally, by Ctrl-C, or by a
   lost connection that is not recovered), **Then** the helper pod rtop created on the node is
   deleted, and if deletion fails rtop prints the pod's name so the user can remove it.

---

### User Story 3 - Choose a target interactively (Priority: P3)

The engineer starts rtop in Kubernetes mode without naming a pod or node. rtop lists pods in the
current namespace (status, node, container count) and, on a second tab, the cluster's nodes
(status, roles, pod count). The user filters, picks one, and continues as in Story 1 or 2.

**Why this priority**: pod names change on every rollout; picking removes copy-paste but is not
needed to get value from Stories 1 and 2.

**Independent Test**: start rtop in Kubernetes mode with no target; pick a pod, then quit and pick
a node; each opens the matching view.

**Acceptance Scenarios**:

1. **Given** a namespace with running pods, **When** the user starts rtop in Kubernetes mode with
   no target, **Then** a picker lists the pods and nodes and accepts `/` filtering.
2. **Given** the picker, **When** the user selects a pod that is not Running or a node that is not
   Ready, **Then** rtop explains why it cannot attach instead of failing silently.

---

### User Story 4 - Container-aware dashboard and lessons (Priority: P4)

When attached to a pod, the dashboard shows what each container is allowed to use, not only what
the node has: CPU and memory requests and limits, usage against the limit, throttling, OOM kills,
restart count and node. The explain panel teaches the classic misreadings, for example that
`/proc/meminfo` and the CPU count inside a container show the node's totals, not the limit.

**Why this priority**: this is what makes the feature a learning tool rather than a remote htop;
it depends on Story 1.

**Independent Test**: attach to a pod with a 256 MiB memory limit and a 500m CPU limit; the
dashboard shows those limits and usage against them, and the memory explanation describes why node
totals appear inside the container.

**Acceptance Scenarios**:

1. **Given** a pod whose containers have CPU and memory limits, **When** rtop is attached, **Then**
   the dashboard shows each container's usage against its limit and names the cgroup files read.
2. **Given** a container that has been CPU-throttled or OOM-killed, **When** rtop is attached,
   **Then** the dashboard shows the throttled periods and OOM kills since the container started.

---

### Edge Cases

- The kubeconfig context is unreachable, expired or lacks permission: rtop exits with a message
  naming the context and the missing permission (for example `create pods/exec` in namespace X),
  not a stack trace.
- A container lacks the tools and the user may not add ephemeral containers: that container's rows
  are replaced by one line saying why it cannot be read; the other containers still show.
- Ephemeral containers are disabled or unsupported on the cluster: same handling as above, with
  the reason.
- The pod is deleted or rescheduled, or a container restarts while attached: rtop shows that the
  target is gone or restarted (with the new restart count) and offers to reattach.
- The network to the API server drops: the screen keeps the last data, marks it stale with its age,
  and retries without exiting.
- The pod shares one PID namespace (`shareProcessNamespace: true`): processes are listed once (no
  duplicates), each labelled with its container, plus the pause/infra process.
- Init containers that have finished: not listed as running; their final state is visible in the
  pod status.
- Node mode where the namespace forbids privileged pods (Pod Security admission) or the node is
  tainted against the helper pod: rtop explains which policy or taint blocked it and how to choose
  another namespace.
- Node mode and rtop is killed hard (SIGKILL, laptop sleeps): the helper pod removes itself after
  a bounded lifetime, and it carries a label so leftovers can be found.
- Processes rtop cannot read (other UID, restricted `/proc/<pid>` files): the same "restricted"
  handling as local runs, with the reason shown.
- Windows nodes and containers: not supported; rtop says so.
- Labs (`rtop lab ...`) in Kubernetes mode: not supported in this feature; rtop says so.

## Requirements *(mandatory)*

### Functional Requirements

**Targets and access**

- **FR-001**: Users MUST be able to start rtop with a Kubernetes pod as target (pod name;
  optionally namespace, container and kubeconfig context) or a node as target (node name;
  optionally context). Unspecified values default to the current kubeconfig context and namespace.
- **FR-002**: rtop MUST reach pods and nodes only through the Kubernetes API server with the user's
  kubeconfig credentials, with no SSH or direct node access.
- **FR-003**: For each container, rtop MUST first read process data through exec using tools
  already in the image, adding nothing to the pod. Only when the image lacks them MUST it fall back
  to an ephemeral debug container targeting that container, and it MUST tell the user it did so.
  A start option MUST allow forbidding the fallback.
- **FR-004**: For a node target, rtop MUST read the node's processes through a short-lived helper
  pod scheduled on that node with access to the host's process namespace. The helper pod MUST be
  deleted when rtop exits, MUST remove itself after a bounded lifetime if rtop disappears, and MUST
  carry a label identifying it as rtop's.

**Views**

- **FR-005**: A pod target MUST show the processes of all running containers of the pod in one
  table with a CONTAINER column; the user MUST be able to narrow to one container at start or
  inside rtop without restarting.
- **FR-006**: A node target MUST show all node processes with POD (namespace/name) and CONTAINER
  columns, filled for processes belonging to a container and `host` otherwise.
- **FR-007**: In both modes rtop MUST show the same table, tree, detail tabs, explain panel and raw
  viewer as a local Linux run, with each value's provenance naming the `/proc` path read and the
  container (or host) it was read in.
- **FR-008**: The header MUST always show the target: context, and namespace/pod or node, plus the
  node a pod runs on.
- **FR-009**: When no target is named, rtop MUST offer a filterable picker of pods (current
  namespace) and nodes.
- **FR-010**: For a pod target, the dashboard MUST show each container's CPU and memory requests and
  limits, usage against the limit, throttling, OOM kills and restart count, each with its source.
  For a node target, the dashboard MUST show the node's resources as in a local run, plus
  allocatable capacity and pod count.

**Safety and errors**

- **FR-011**: Kubernetes mode MUST be read-only by default. Sending signals and renicing MUST be
  disabled unless the user starts rtop with an explicit opt-in, and each action MUST then be
  confirmed with the target (pod/container or node) named in the prompt.
- **FR-012**: Failures (unreachable cluster, missing permission, pod not running, node not Ready,
  container restarted, policy blocking the helper pod, connection lost) MUST produce a message
  naming the cause and next step. A lost connection MUST mark data stale and retry, not exit.
- **FR-013**: rtop MUST NOT store or print cluster credentials. The README MUST list the exact
  permissions each mode needs, and rtop MUST NOT need more.
- **FR-014**: The explain catalog MUST cover every new field and describe both the in-container or
  host source and the Kubernetes source (pod spec, pod status, node status).

### Key Entities

- **Target**: what rtop is attached to: local machine; pod (context, namespace, pod, node, optional
  container filter); or node (context, node). Shown in the header and used for provenance.
- **Container**: name, how it is read (exec or ephemeral debug container), state, restart count,
  last termination reason, requests and limits.
- **Process (Kubernetes mode)**: a process as today, plus the container (and pod, on a node) it
  belongs to; within a pod, processes from different containers may share a PID number.
- **Helper pod (node mode)**: the short-lived pod rtop places on a node; name, namespace, label,
  lifetime.
- **Container resource usage**: cgroup CPU usage, throttled periods, memory current/max, OOM events.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On a healthy cluster, a user sees a pod's process table in under 5 seconds and a
  node's in under 10 seconds from the command line.
- **SC-002**: The process list for a pod matches `ps` run inside each container for 100% of
  processes on each refresh (ignoring processes that start or exit between the two samples); on a
  node, 100% of processes of known pods are attributed to the right pod and container.
- **SC-003**: A refresh tick adds no more than 1% CPU, on average, to each target container or to
  the node.
- **SC-004**: After rtop exits in node mode (normally or by Ctrl-C), no rtop helper pod remains;
  after a hard kill, none remains past the bounded lifetime.
- **SC-005**: Every failure listed in FR-012 shows a message naming the cause; none ends in a panic
  or a raw error dump.
- **SC-006**: A user new to containers can answer "why does `free` in my pod show 64 GiB when the
  limit is 256 MiB?" using only rtop's explain panel.

## Assumptions

- Users already have a working kubeconfig and cluster access; rtop reuses it (including exec-based
  credential plugins) and does not log in.
- Target nodes and containers run Linux; cgroup v2 is the primary target, v1 shows what it can.
- Permissions per mode (to be listed in the README):
  - pod via exec: `get`/`list` pods, `create` `pods/exec`;
  - pod with fallback: additionally `patch` `pods/ephemeralcontainers`;
  - node: `get`/`list` nodes and pods, `create`/`delete` pods in the helper namespace, and that
    namespace must allow privileged pods with host PID access.
- Ephemeral debug containers stay in the pod spec until the pod is replaced; this is a Kubernetes
  property, documented in the README and shown to the user when the fallback is used.
- The debug container and the helper pod use a published rtop image; its location is a plan-level
  decision.
- The node helper pod runs in the current namespace unless the user names another.
- Refresh interval in Kubernetes mode defaults to 2 seconds (local: 1 second); the user can change
  it.
- Out of scope: several pods or nodes at once, labs in Kubernetes mode, port-forwarding, log
  viewing, and editing workloads.
- Tests use a local cluster (kind or minikube) in Docker plus recorded fixtures, so the normal
  test suite does not need a cluster.
