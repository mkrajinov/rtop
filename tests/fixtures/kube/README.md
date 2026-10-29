# Kubernetes fixtures

Byte streams in the reader wire format (specs/001-k8s-targets/contracts/reader-wire.md) and
API objects used by the `src/kube` tests.

- `hello.bin`, `snap-pod.bin`, `details.bin`: hand-written, small, with the edge cases a capture
  rarely has (an unreadable `fd/`, a non-root UID, an unreadable `environ`).
- `kind-*.bin`: captured from busybox in the `multi` pod of the kind cluster (`make kube-up`):
  `(cat src/kube/reader.sh; sleep 1; printf 'snap 1\n'; …) | kubectl exec -i … -- sh`, split at
  each `\x1eD <seq>` marker.
- `pod*.json`: `kubectl get pod -o json` of the demo pods in tests/kube/workloads.yaml.
- `kind-node.bin`: hello, `snap 1` and `cmd 2 1 130 579 744` from a hostPID busybox pod on the
  kind node, with the node-mode prelude (`RTOP_ROOT=/proc/1/root`, `RTOP_NET=/proc/1/net`).
- `pods-on-node.json`, `node.json`: `kubectl get pods -A --field-selector spec.nodeName=… -o json`
  and `kubectl get node -o json` from the same cluster, trimmed to the fields rtop reads.
- `cgroup-paths.txt`: `/proc/<pid>/cgroup` lines from that node plus other runtimes and drivers,
  each with the pod and container ID it should yield.
