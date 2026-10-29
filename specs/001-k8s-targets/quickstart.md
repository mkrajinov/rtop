# Quickstart: validate Kubernetes targets end to end

Every command passes `--context kind-rtop`. Never run these against another context.

## Prerequisites

- Docker running; `kind` and `kubectl` on PATH (kubectl only checks results; rtop does not
  use it).
- rtop built with default features (Kubernetes included): `cargo build --release`.

## 1. Cluster and sample workloads

```sh
kind create cluster --name rtop
kubectl --context kind-rtop apply -f tests/kube/workloads.yaml
kubectl --context kind-rtop wait --for=condition=Ready pod --all -n rtop-demo --timeout=120s
```

`tests/kube/workloads.yaml` (created in tasks) defines, in namespace `rtop-demo`:

| Pod | Containers | Purpose |
|---|---|---|
| `multi` | `app` (busybox `sleep`), `sidecar` (busybox loop) | Story 1: two PID namespaces, two PID 1s |
| `distroless` | `app` (`gcr.io/distroless/static` + a static sleeper) | debug-container fallback |
| `shared` | `app`, `sidecar`, `shareProcessNamespace: true` | one namespace, cgroup attribution, `pause` |
| `limited` | `app` with 256Mi / 500m limits, a CPU burner | Story 4: throttling, usage vs limit |

The namespace is labelled `pod-security.kubernetes.io/enforce=privileged` for the node helper.

## 1a. Picker (Story 3)

```sh
rtop k8s --context kind-rtop -n rtop-demo
```

Expect namespaces on the left with `rtop-demo` selected, its four pods on the right with
READY, STATUS, RESTARTS, AGE and NODE. Tab switches panes, `/` filters the focused pane,
Enter on `multi` opens the pod view, Esc there returns to the picker with `multi` selected.
"all namespaces" adds a NAMESPACE column.

## 2. Pod, all containers (Story 1, SC-001, SC-002)

```sh
time rtop k8s --context kind-rtop --pod rtop-demo/multi
```

Expect:
- the table within 5 s, with a CONTAINER column showing `app` and `sidecar`;
- two PID 1 rows, and Enter on each opens the right one;
- the header showing `kind-rtop · rtop-demo/multi · node rtop-control-plane`.

Compare against:
```sh
for c in app sidecar; do kubectl --context kind-rtop -n rtop-demo exec multi -c $c -- ps -o pid,comm; done
```

## 3. Distroless fallback (FR-003)

```sh
rtop k8s --context kind-rtop --pod rtop-demo/distroless
kubectl --context kind-rtop -n rtop-demo get pod distroless -o jsonpath='{.spec.ephemeralContainers[*].name}'
```

Expect:
- a notice that debug container `rtop-app-xxxx` was added, then the app's processes;
- quitting and restarting reuses the same debug container (the name list does not grow).

With `--no-debug-container`, the `app` container shows one line saying why it can't be read.

## 4. Shared PID namespace

`rtop k8s --context kind-rtop --pod rtop-demo/shared` shows each process once, labelled `app`,
`sidecar` or `pause`.

## 5. Node (Story 2, SC-004)

```sh
rtop k8s --context kind-rtop --node rtop-control-plane --helper-namespace rtop-demo
```

Expect:
- the table within 10 s, with kubelet and containerd shown as `host`;
- `rtop-demo/multi` processes labelled with pod and container;
- `/multi` filters to that pod.

After quitting:
```sh
kubectl --context kind-rtop -n rtop-demo get pods -l app.kubernetes.io/managed-by=rtop
```
returns nothing, also after SIGTERM to rtop and after Esc back to the picker. After `kill -9`
of rtop, the helper is gone within its 3600 s deadline. To test quickly, run once with an env
override `RTOP_HELPER_DEADLINE=40`: when the helper ends, rtop deletes it and reads through a new
one.

Admission: in a namespace labelled `pod-security.kubernetes.io/enforce=baseline`, rtop exits
with `helper pod rejected by admission in <ns>: … violates PodSecurity …` and the
`--helper-namespace` hint.

## 6. Container dashboard (Story 4, SC-006)

`rtop k8s --context kind-rtop --pod rtop-demo/limited` shows:
- a containers card first: `app  cpu 0.50 / 0.5  thr 100%  mem 2.6M / 256M  oom 0  ↻ 0`;
- the memory card titled `memory · node`, and the node's CPU card in the second row;
- `L` opens the first lesson, `container_meminfo`: `/proc/meminfo` in a container shows the node.

`rtop k8s --context kind-rtop --pod rtop-demo/oom` (48Mi, grows 1 MiB/s) shows memory rising to
the limit, then `↻ N last OOMKilled` after each restart.

## 7. Failures (FR-012, SC-005)

| Do | Expect |
|---|---|
| `--context nope` | `context nope: …` and exit |
| `--pod rtop-demo/missing` | `pod rtop-demo/missing not found` |
| `kubectl --context kind-rtop -n rtop-demo delete pod multi` while attached | target gone, offer to reattach |
| `docker pause rtop-control-plane` for 10 s while attached | data marked stale with its age, recovers after unpause |

## 8. Target load (SC-003)

While attached to the node:
```sh
kubectl --context kind-rtop -n rtop-demo top pod -l app.kubernetes.io/managed-by=rtop
```
The helper uses about 10m CPU (1% of one core) on kind at the node default of 3 s. Without
metrics-server, read its `/sys/fs/cgroup/cpu.stat` (`usage_usec`) twice with `kubectl exec`.

## Cleanup

```sh
kind delete cluster --name rtop
```
