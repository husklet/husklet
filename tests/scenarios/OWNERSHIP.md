# Scenario ownership

The direct child folders containing `test.yaml` are repository end-to-end
tests. Each definition owns its local `source/`, `input/`, and `golden/` files;
removing that folder removes the complete scenario.

Rust tests of one crate's public API belong in that crate's `tests/` target.
Small behavioral tests of private source entities stay inline beside their
production source. Detached Rust registries, runners, fixtures, and workflow
modules do not belong under this YAML discovery root.

Current classification:

| content | owner | state |
|---|---|---|
| Direct children with `test.yaml` | Repository YAML E2E | Active and discovered by `testing scenarios` |
| `groups/` | Crate public-contract tests | Removed after the final observability behavior moved to `hl-client` |
| `workflows/` | Mixed crate contracts and multi-container E2E | Detached; tracked in `WORKFLOW_AUDIT.md` until each remaining behavior has an executable owner |
| Root audit documents | Migration evidence | Retained until the detached workflow closure completes |

There is no active `registry/`, `main/`, `harness/`, root `fixtures/`, or root
`golden/` directory. The Rust testing application under `src/apps/testing`
provides discovery, scheduling, execution, and reporting; it does not duplicate
scenario declarations.
