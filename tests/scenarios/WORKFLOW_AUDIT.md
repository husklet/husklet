# Detached workflow ownership audit

The Rust files under `tests/scenarios/workflows/` are not compiled or invoked by
the repository testing application. They were detached when the former scenario
binary was removed. The `testing scenario-workflows` command only prints a
second, hard-coded list; it does not make these workflows executable.

Detachment is not a deletion gate. Each behavior must first move either to the
public-contract tests of the package that owns it or to a direct child of
`tests/scenarios/` with a discoverable `test.yaml`. The future provider and cache
pipeline in `tests/PIPELINE.md` is a proposal and is not an implementation target
for this closure.

## Ownership matrix

| Detached workflow | Durable owner | Current closure |
|---|---|---|
| Former smoke workflow | `tests/scenarios/smoke-realimage/` | Exact image, ISA, command, marker, timeout, and output coverage exists for all three rows; the detached module and registration are deleted. |
| Former software workflow | `tests/scenarios/{databases,languages}/` | Exact Redis, PostgreSQL, NATS, and Python observations are folder-owned with local goldens and oracle mappings; the detached module and registration are deleted. |
| `pty.rs` | `tests/scenarios/terminal/` | Terminal allocation and termios behavior exists, but the five retained cases drive an attached session with timed input. The current YAML action model cannot express attached interactive input, so this workflow is not closed. |
| `network.rs` | `hl-container` public network/runtime contract | Durable IPAM, aliases, multiple attachments, removal, metadata, unrelated-network isolation, distinct allocation, and complete topology teardown are covered by `hl-container/tests/networks.rs`; live name routing is covered by the daemon bridge test. Live address routing remains detached, so this module and its registration are retained. |
| `compose.rs` | Container/network/volume public contracts plus repository multi-container E2E | Labels, volumes, endpoints, aliases, and atomic multi-network attachment have package coverage. The live two-network routing topology remains a repository-level E2E gap because the current YAML runner creates one container per case. |
| Former Docker workflows | `hl-client`, `hl-daemon`, and `hl-container` public-contract tests | `DOCKER_WORKFLOW_AUDIT.md` maps the redundant sweep. Successful root-filesystem import and requested-tag discovery moved to `hl-client/tests/daemon/image.rs`; both workflow registrations and detached modules are deleted. |
| `build.rs` and `build/` | `hl-images` parsing/model tests and `hl-daemon` builder/API tests | Parsing, context safety, copy mechanics, metadata, and cache identity have package coverage. Full build execution, cache reuse, concurrent publication, multistage copying, run mounts, and resulting-image execution still require durable integration owners. |
| `fixture.rs` | No independent behavior | Delete after its final workflow consumer moves. |
| `mod.rs` | No durable owner | Delete after all named behaviors close; then remove the hard-coded CLI command and inventory field rather than retaining a second registry. |

## Required deletion order

1. Move or map one coherent behavior domain and record its exact replacement.
2. Verify the replacement from the exact committed tree.
3. Delete only the corresponding detached module and its dispatch/list entry.
4. Delete `fixture.rs`, `mod.rs`, `ScenarioWorkflows`, and
   `legacy_workflows` only when no detached workflow remains.

This order prevents a dead source file from being mistaken for coverage while
also preventing the inventory command from advertising tests that cannot run.
