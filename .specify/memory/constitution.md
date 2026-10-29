# rtop Constitution

## Core Principles

### I. Provenance for Every Value (NON-NEGOTIABLE)

Every value rtop displays MUST carry its provenance: the exact Linux file and field
(for example `/proc/PID/stat field 14`) or the macOS syscall, struct and member
(for example `proc_pidinfo(PROC_PIDTASKINFO).pti_total_user`). The raw source behind a
value MUST be reachable from the UI (raw viewer or the source line of the detail view).
Dashboard cards MUST name their data source in the title or README sources table.

Rationale: rtop exists to teach where numbers come from. A value without a source is
monitoring, not learning.

### II. Teach First

Every new field key MUST have an entry in `assets/explain.toml` covering what it means,
its Linux and macOS sources, common misreadings and a "try it" command; the catalog test
MUST stay green. Features are prioritised in `ROADMAP.md` by what they teach about the
kernel, not only by monitoring value. Derived numbers (rates, percentages) MUST explain
how they are derived (for example: delta between two samples).

Rationale: the explain panel, raw viewer, labs and kernel views are the product.

### III. Both Platforms, Side by Side

Features MUST work on macOS and Linux, or show on the unsupported platform an explicit
message saying why it is unavailable (never a silent blank). Platform code lives in
`src/collector/{macos,iokit,linux}.rs`; Linux parsing lives in
`src/collector/linux_parse.rs`, does no I/O, and is tested on every OS against
`tests/fixtures/linux`. Missing hardware (no battery, no GPU, no fans) MUST degrade to
`None`/hidden, never to an error or panic.

Rationale: the Mac is the development machine; showing both mechanisms is how working on
macOS still teaches Linux.

### IV. Deterministic Tests

UI changes MUST be covered by `insta` snapshots rendered from the fixture collector,
never from live data. Snapshot updates MUST be deliberate and reviewed. Pure logic (CPU
and rate deltas, parsers, formatting) MUST have unit tests. Before a change is done,
`make test lint` MUST pass on macOS and `make linux-test` MUST pass in Docker.

Rationale: system data changes every second; only fixtures make the UI testable.

### V. Cheap and Safe to Run

rtop MUST stay at about 1% CPU when idle and MUST NOT redraw faster than input or the
refresh tick require (dirty flag, drain queued events, draw once). Collection MUST NOT
block the UI on slow sources. The TUI process MUST NOT `fork`; labs re-exec rtop as
`__lab-worker` in their own process group, are killed with the group when rtop exits,
and exit on their own after a timeout. rtop is read-only except for explicit user
actions (signal, renice) and labs. In Kubernetes mode rtop may add to the cluster only what
reading requires (an ephemeral debug container, a node helper pod); each addition MUST be
labelled as rtop's, have a bounded lifetime, be announced to the user, and be possible to
disable.

Rationale: a monitor that distorts the system it measures teaches the wrong lesson.

## Technical Constraints

- Rust stable, edition 2024; `ratatui` + `crossterm` for the TUI, `clap` for the CLI.
- New dependencies need a reason a few lines of code cannot meet; macOS FFI (IOKit, SMC,
  HID) is raw `extern "C"` confined to collector modules, with owned wrappers that
  release resources on `Drop`.
- Heavy dependencies that serve one optional mode (for example an async runtime and a
  Kubernetes client) MUST sit behind a Cargo feature, confined to that mode's module and
  thread; `cargo build --no-default-features` MUST still produce a working local-only rtop.
- No network access at runtime, except Kubernetes mode, which talks only to the API server of
  the kubeconfig context the user selected, through rtop's own Kubernetes module (kube-rs,
  feature `kube`). No telemetry.
- Colours are exact Tailwind CSS v4 palette values, chosen through the theme system.
- UI accents never use borders or colour stripes flush against an edge; selection uses a
  background highlight, accents are inset glyphs or detached pills.
- eBPF features are Linux-only, opt-in, and MUST NOT be required for the `/proc` views.

## Development Workflow

- New roadmap features go through Spec Kit: `/speckit-specify` → (`/speckit-clarify`)
  → `/speckit-plan` → `/speckit-tasks` → `/speckit-implement`, with specs in `specs/`.
- Commits follow Conventional Commits (`feat:`, `fix:`, `docs:` …) on `main` or a
  feature branch.
- A feature is done when: tests and lint pass on both platforms, the explain catalog and
  README sources table are updated, and its `ROADMAP.md` checkbox is ticked.

## Governance

This constitution supersedes other practices for rtop. Plans produced by `/speckit-plan`
MUST include a constitution check, and any violation MUST be justified in the plan's
complexity tracking. Amendments are made with `/speckit-constitution` and versioned with
semantic versioning: MAJOR for removing or redefining a principle, MINOR for adding a
principle or section, PATCH for wording. Runtime guidance lives in `README.md` and
`ROADMAP.md`.

**Version**: 1.2.0 | **Ratified**: 2026-10-29 | **Last Amended**: 2026-10-29
