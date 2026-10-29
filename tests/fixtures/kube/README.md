# Kubernetes fixtures

Byte streams in the reader wire format (specs/001-k8s-targets/contracts/reader-wire.md) and
API objects used by the `src/kube` tests.

- `hello.bin`, `snap-pod.bin`, `details.bin`: hand-written, small, with the edge cases a capture
  rarely has (an unreadable `fd/`, a non-root UID, an unreadable `environ`).
- `kind-*.bin`: captured from busybox in the `multi` pod of the kind cluster (`make kube-up`):
  `(cat src/kube/reader.sh; sleep 1; printf 'snap 1\n'; …) | kubectl exec -i … -- sh`, split at
  each `\x1eD <seq>` marker.
- `pod*.json`: `kubectl get pod -o json` of the demo pods in tests/kube/workloads.yaml.
