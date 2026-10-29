# Contract: reader wire protocol

The reader is a POSIX `sh` script that the kube thread writes as the first stdin input of an
exec session running `sh` in the target (`ExecOpen` with `stdin_init`, see kube-messages.md).
The bytes move as `StreamWrite` / `Event::StreamData` messages. It defines functions, prints a
hello frame, then loops reading one request per line. The same protocol is used in exec,
debug-container and helper-pod sessions. A future binary reader (research R10) must speak it
unchanged.

## Framing

Every response is a sequence of frames:

```text
\x1eF <name>\n          frame start; <name> is a path or a key
<payload bytes>
\x1eE <rc>\n            frame end; rc = exit status of the read (0 ok, non-zero = unreadable)
```

A request ends with `\x1eD <seq>\n` (done). `<seq>` echoes the request's sequence number, so
late responses after a timeout are discarded.

- `\x1e` (ASCII record separator) does not occur in `/proc` text files. `cmdline` and
  `environ` are arbitrary bytes. If a payload contains `\x1e` followed by `E `, the parser
  sees a short frame. This is accepted and documented: the row's command shows truncated, and
  nothing else breaks.
- Payloads are raw bytes; the parser works on `&[u8]` and converts with lossy UTF-8 per field,
  as local mode does.

## Requests

| Request | Response frames |
|---|---|
| `hello` (sent by the script itself on start, seq 0) | `version` (`1`), `pid` (the reader's own PID, labelled in the table), `uname` (`uname -r` or empty), `passwd` (`/etc/passwd`), `pagesize` (`getconf PAGESIZE`, often missing), `statm` and `status` of the reader (rtop derives the page size from them when `getconf` is missing) |
| `snap <seq> [cgroup-root]` | `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/uptime`, `/proc/net/dev`, `/proc/diskstats`, then one `p <pid>` frame per process, then cgroup files under `cgroup-root` if given |
| `cmd <seq> <pid>…` | `/proc/<pid>/cmdline` and `/proc/<pid>/cgroup` frames per pid (the cgroup tells containers apart in a shared PID namespace) |
| `cg <seq> <pid>…` | one `/proc/<pid>/cgroup` frame per pid (node mode attribution) |
| `details <seq> <pid>` | `status`, `cmdline`, `environ`, `io`, `limits`, `cgroup`, `schedstat`, `smaps_rollup`, `maps`, `wchan`, `oom_score`, `oom_score_adj`, links `exe`, `cwd`, `ns/*`, `fd/*` (as `L <fd> <target>` lines), `task/*/stat`, `net/tcp`, `tcp6`, `udp`, `udp6`, `unix` |
| `kill <seq> <SIG> <pid>` | `rc` frame with the exit status and stderr of `kill` |
| `renice <seq> <n> <pid>` | `rc` frame |
| `bye` | the script exits |

`p <pid>` payload: three lines, `<stat line>`, `<uid>` (from the `Uid:` line of `status`,
real UID) and `<fd count>` (`-1` when `fd/` is unreadable).

The link frames come from `readlink`. If `readlink` is missing, the shell reports
`ls -l` output, which the parser also accepts.

## Timeouts

- **`snap`**: no `D` within 2 × the refresh interval marks the session Stale. The request is
  not resent until the next tick.
- **`details`**: no `D` within 5 s shows "details timed out" for that process.
- **Dead session**: `Event::StreamClosed`, or `Event::ThreadRestarted`, starts a reconnect with
  backoff.

## Fixtures

`tests/fixtures/kube/` holds captured responses (`hello.bin`, `snap-pod.bin`, `snap-node.bin`,
`details.bin`, `pod.json`, `pods-on-node.json`, `cgroup-paths.txt`). The parser
(`src/kube/wire.rs`) and attribution (`src/kube/attribution.rs`) are pure functions tested
against them on every OS (Principle III, IV).
