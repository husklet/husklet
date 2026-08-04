# Language scenario category oracle

This category owns schema-compatible language workloads as one discoverable
scenario domain. It consolidates the previously prepared .NET and Elixir cases
with the declarative Perl, PHP, Ruby, Python, and Node cohorts while preserving
every stable case ID independently.

The migration sources studied were the former `languages-dotnet/test.yaml`, the
legacy `fixtures/languages-{node,perl,php,python,ruby}.yaml` manifests, the
language aggregation and uniqueness check in `groups/languages.rs`, and each
matching row in `golden/scenario.contracts` and `golden/images.manifest`.
Commands remain PATH-resolved by using `actions.argv`; output checks use local,
bounded, path-safe golden files containing the exact legacy substring bytes.
The .NET fixtures and native diagnostic settings remain unchanged.

No retained C runtime mechanism changes in this data ownership move. The C
engine remains a behavioral execution oracle for the same guest programs, but
no C entry point, state owner, lock, teardown transition, syscall ordering rule,
host branch, or architecture branch is being ported.

Focused acceptance uses the single category selector on each guest ISA:

```text
cargo run -p testing -- scenarios languages --isa arm64
cargo run -p testing -- scenarios languages --isa amd64
```

Acceptance requires every selected stable ID to retain its source image,
command, timeout, class, target set, expected-failure state, and output
substring. Legacy rows removed from the worktree remain auditable through this
report and repository history; do not claim a dual run when the old row is no
longer executable from the exact tree.
