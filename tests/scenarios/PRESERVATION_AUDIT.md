# Remaining legacy scenario preservation

This audit prevents declarative fixture cleanup from deleting contracts that do
not yet have a folder-owned equivalent. It compares the remaining manifests at
`747c2b3d0` with each category's `test.yaml` by stable ID. These manifests and
their Rust groups must remain until every row and group-only behavior below has
an executable owner.

## Old-only declarative contracts

| Category | Legacy | Folder YAML | Old-only IDs | Blocking ownership gap |
|---|---:|---:|---|---|
| `weird` | 54 | 53 | `static-nonpie-helloworld` | Execution requires the image-configured entrypoint. |

The suffix in the table is joined to its category with `/`. There is 1
old-only stable IDs in total. A larger but differently named workload is not a
replacement: preservation requires the same ID, image, action semantics,
targets, expected-failure metadata, timeout, and output oracle.

## Rust group-only behavior

| Group | Non-manifest behavior | Required durable owner |
|---|---|---|
| `groups/weird.rs` | `test_expected_failures` proves AMD64 alone expects `weird/dotnet-ryujit` to fail. | The folder YAML loader's target/xfail validation and a focused inventory assertion. |

No API-behavior module was deleted by the declarative closure batches.

The per-category `ORACLE.md` files retain the detailed commands, readiness
contracts, entrypoint limitation, scheduler differences, and existing owner
mappings. This inventory is a deletion gate, not a claim that the remaining
legacy harness is a desirable permanent owner.
