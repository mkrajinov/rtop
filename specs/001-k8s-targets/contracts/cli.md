# Contract: command line

Kubernetes mode is selected by `--pod`, `--node` or `--kube`. Without them rtop behaves as today.

```text
rtop --pod [NS/]POD [-c CONTAINER] [-n NS] [--context CTX]
rtop --node NODE [--helper-namespace NS] [--context CTX]
rtop --kube [-n NS] [--context CTX]          # picker (Story 3)
```

| Flag | Default | Meaning |
|---|---|---|
| `--pod [NS/]POD` | | Pod target. `NS/` overrides `-n`. |
| `-c, --container NAME` | all containers | Start narrowed to one container (FR-005). |
| `--node NODE` | | Node target (Story 2). |
| `--kube` | | Open the pod and node picker. |
| `-n, --namespace NS` | kubeconfig namespace, else `default` | Pod namespace; picker namespace. |
| `--helper-namespace NS` | `-n` value | Where the node helper pod is created. |
| `--context CTX` | kubeconfig current context | Resolved once; passed to every kubectl call. |
| `--debug-image IMAGE` | `busybox:1.37` | Image for the debug container and the helper pod. |
| `--no-debug-container` | off | Never add an ephemeral container; unreadable containers are reported instead. |
| `--allow-actions` | off | Enable signals and renice in Kubernetes mode (FR-011). |
| `--kubectl PATH` | `kubectl` on PATH | kubectl binary to use. |
| `-d, --delay SECS` | 2.0 in Kubernetes mode (1.5 locally) | Refresh interval. |

Rules:
- `--pod`, `--node` and `--kube` are mutually exclusive. `lab` with any of them is an error:
  "labs are not available in Kubernetes mode".
- At start rtop prints nothing to the cluster except reads, until a fallback or helper is
  needed. The header then shows what was added.

Exit messages (FR-012), one line each plus the next step:

| Situation | Message starts with |
|---|---|
| kubectl missing | `kubectl not found` (`--kubectl` hint) |
| context unknown or unreachable | `context <ctx>: …` |
| RBAC denied | `forbidden: <verb> <resource> in <ns>` (link to README permissions) |
| pod not Running / node not Ready | `pod <ns/name> is <phase>: <reason>` / `node <name> is NotReady` |
| helper pod rejected | `helper pod rejected by <admission>: …` (`--helper-namespace` hint) |
