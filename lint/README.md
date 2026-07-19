# Design lint review

`make lint-cases` rebuilds this queue for the one- or two-argument free-function rule only.

- `errors/` contains unclassified findings.
- `check/` contains functions marked with `#[hl_design::classify(...)]`.

Both queues are flat. Files use
`<unix_timestamp>_<domain>_<package>_<function>.md`; every component after the timestamp is snake_case.

Approved patterns accumulate in `examples/positive.md`. Rejected patterns and their failure modes accumulate
in `examples/negative.md`. Subagents must consult both before proposing or implementing a case.

Classification is temporary triage. Review related cases together, extract a cohesive entity or package when
the concept becomes clear, then remove the annotation. Regeneration deletes `errors/` and `check/`; keep
durable decisions in source and architecture documentation.
