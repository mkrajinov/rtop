# Contract: command line

Kubernetes mode is the `k8s` subcommand. Every Kubernetes flag belongs to it; plain `rtop`
behaves as before and rejects them.

```text
rtop k8s [-n NS] [--context CTX]                                  # picker (Story 3)
rtop k8s --pod [NS/]POD [-c CONTAINER] [-n NS] [--context CTX]
rtop k8s --node NODE [--helper-namespace NS] [--context CTX]      # Story 2
```

Without `--pod` (or `--node`), `rtop k8s` opens the picker on the context: namespaces on the
left, the selected namespace's pods on the right. Enter opens the pod view; Esc in the pod view
returns to the picker. If the user may not list namespaces, the picker shows the context's
namespace and `/` accepts a typed namespace name.

| Flag | Default | Meaning |
|---|---|---|
| `--pod [NS/]POD` | | Pod target. `NS/` overrides `-n`. |
| `-c, --container NAME` | all containers | Start narrowed to one container (FR-005). Needs `--pod`. |
| `--node NODE` | | Node target (Story 2). |
| `-n, --namespace NS` | kubeconfig namespace, else `default` | Namespace of `--pod`; the namespace the picker starts in. |
| `--helper-namespace NS` | `-n` value | Where the node helper pod is created. |
| `--context CTX` | kubeconfig current context | Resolved once when the client is built; no other context is used. Always shown in the header, highlighted when it was not given explicitly. |
| `--debug-image IMAGE` | `busybox:1.37` | Image for the debug container and the helper pod. |
| `--no-debug-container` | off | Never add an ephemeral container; unreadable containers are reported instead. |
| `--allow-actions` | off | Enable signals and renice in Kubernetes mode (FR-011). |
| `-d, --delay SECS` | 2.0 in Kubernetes mode (1.5 locally) | Refresh interval. Global, like `--theme` and `--accent`. |

Rules:
- `--pod` and `--node` are mutually exclusive. `lab` and `inspect` are separate subcommands,
  so they cannot be combined with Kubernetes mode.
- The picker resolves the context once; every pod view opened from it uses that context by
  name, so a kubeconfig edited meanwhile cannot switch clusters.
- Errors before the TUI starts (context, credentials, a `--pod` that cannot be opened) exit with
  the messages below. A pod chosen in the picker that cannot be opened shows the same message
  in the picker's status line instead.
- At start rtop prints nothing to the cluster except reads, until a fallback or helper is
  needed. The header then shows what was added.

Exit messages (FR-012), one line each plus the next step:

| Situation | Message starts with |
|---|---|
| built without the `kube` feature | `this rtop was built without Kubernetes support` (rebuild with default features) |
| no kubeconfig / login plugin failed | `kubeconfig: …` / `credentials for <ctx>: …` |
| context unknown or unreachable | `context <ctx>: …` |
| RBAC denied | `forbidden: <verb> <resource> in <ns>` (link to README permissions) |
| pod not Running / node not Ready | `pod <ns/name> is <phase>: <reason>` / `node <name> is NotReady` |
| helper pod rejected | `helper pod rejected by <admission>: …` (`--helper-namespace` hint) |
