# Remaining legacy scenario preservation

This audit prevents declarative fixture cleanup from deleting contracts that do
not yet have a folder-owned equivalent. It compares the remaining manifests at
`747c2b3d0` with each category's `test.yaml` by stable ID. These manifests and
their Rust groups must remain until every row and group-only behavior below has
an executable owner.

## Old-only declarative contracts

| Category | Legacy | Folder YAML | Old-only IDs | Blocking ownership gap |
|---|---:|---:|---|---|
| `databases` | 44 | 39 | `mongo-agg-7`, `mongo-count-7`, `mongo-filter-count-7`, `mongo-parallel-readiness-7`, `mongo-version-8` | Ordered readiness, retry delay, and failure-log capture must remain executable. |
| `languages` | 12 | 43 | `go-fib-123-alpine`, `go-sum-122-alpine`, `go-sum-122-bookworm`, `go-version-122-alpine`, `java-fib-21`, `java-sum-17`, `java-sum-temurin21`, `java-sum-temurin21-alpine`, `java-version-temurin17`, `rust-fib-1-slim`, `rust-sum-1-alpine`, `rust-version-1-slim` | The compiled Go, Java, and Rust cohort has no folder-owned stable-ID mapping. |
| `utilities` | 302 | 301 | `hello-world` | Execution requires the image-configured entrypoint. |
| `weird` | 54 | 53 | `static-nonpie-helloworld` | Execution requires the image-configured entrypoint. |

The suffixes in the table are joined to their category with `/`. There are 19
old-only stable IDs in total. A larger but differently named workload is not a
replacement: preservation requires the same ID, image, action semantics,
targets, expected-failure metadata, timeout, and output oracle.

## Rust group-only behavior

| Group | Non-manifest behavior | Required durable owner |
|---|---|---|
| `groups/databases.rs` | `cleanup_probe` starts a sleeping container, proves bounded wait timeout, force-removes it, and verifies lookup failure. | The typed `hl-container` lifecycle contract cited by `databases/ORACLE.md`; keep the group until the remaining Mongo rows move and the mapping is re-audited. |
| `groups/languages.rs` | `registry_has_every_stable_id_once` asserts 12 unique compiled-language IDs; `run` owns isolated container state for that cohort. | A folder-owned compiled-language cohort plus repository-wide unique-ID validation. |
| `groups/weird.rs` | `test_expected_failures` proves AMD64 alone expects `weird/dotnet-ryujit` to fail. | The folder YAML loader's target/xfail validation and a focused inventory assertion. |

`groups/utilities.rs` adds no behavior beyond loading and running its manifest,
but it cannot be removed while `utilities/hello-world` exists only in that
manifest. No API-behavior module was deleted by the declarative closure batch.

The per-category `ORACLE.md` files retain the detailed commands, readiness
contracts, entrypoint limitation, scheduler differences, and existing owner
mappings. This inventory is a deletion gate, not a claim that the remaining
legacy harness is a desirable permanent owner.
