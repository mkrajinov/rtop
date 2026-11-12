# Specification Quality Checklist: Per-process I/O columns

- [x] Testable against `dd` and `/proc/PID/io`
- [x] No open clarifications (placement follows the disk card; `i` toggles)
- [x] Platform matrix includes Kubernetes and permission gaps
- [x] Scope bounded: `pagecache` lab and Kubernetes reading are later features
