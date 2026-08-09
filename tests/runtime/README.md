# Runtime compatibility tests

Each direct child directory is one removable test application. It owns its
source, image, build recipe, cases, optional oracle, fixtures, and golden bytes.
The runner discovers only direct child directories containing `test.yaml`; no
central registry or Makefile is involved.

```text
<name>/
  test.yaml
  main.c
  fixtures/       # optional, private to this application
  golden/         # exact expected stdout bytes
```

The YAML names the OCI image, guest artifact destination, per-ISA compilers,
compiler flags, oracle commands, and cases. The format is intentionally
unversioned: Husklet has no released compatibility contract for internal test
manifests, so the schema changes directly with the runner.

The engine under test is linked into the runner, so the runner's cargo profile
*is* the engine profile. Case time budgets are set from release timings, and a
sweep refuses to run unless `--engine-profile` matches how the binary was built
(release by default). Build once, then run that binary for every application:

```text
cargo build --release -p testing --bins
./target/release/testing runtime
./target/release/testing runtime core --isa arm64
./target/release/testing runtime core --isa amd64
```

A debug sweep is still available for iteration, and its rows say so:

```text
cargo run -p testing -- runtime core --engine-profile debug
```

Every result row records the profile, and the runner's SHA-256 joins the resume
stamp, so a rebuilt engine can never resume rows measured by another one.

## The corpus mark: `baseline.tsv`

`baseline.tsv` is the last recorded sweep of this corpus. It exists because the
count is not the finding — a *new* failure is — and because a mark that lives
only in a task record makes the next lane re-derive a full sweep to learn a
number nobody wrote down. Its `#`-prefixed header states the commit, profile,
ISAs, row count, `pass`/`fail`/`NOT_RUN` totals, and the host load the sweep ran
under; the rows list every non-pass `(id, target)` with a disposition
(`newly-activated`, `pre-existing`, `pre-existing-flake`, `contention`, `real`,
`inactive`). Any case the file does not list was passing.

The runner reads it, so the diff is a command rather than a manual comparison:

```text
./target/release/testing runtime --baseline
```

That reports every case that moved in either direction — `REGRESSION`, `FIXED`,
`ACTIVATED`, `DEACTIVATED` — and fails the sweep only on a `REGRESSION`, so a
known failing set stays green while a genuinely new failure cannot hide inside
an unchanged total. `--baseline <path>` diffs against another recorded mark.

Record a new mark by taking a full sweep's `--results` ledger, keeping its
non-pass rows, and refreshing the header. Do not record one from a contended
host without saying so in the header: one row in the current mark
(`filesystem/mkfifoat` amd64) is a worker timeout at load 11.36/18, not a
defect.

A case's `environment:` is split by name. An `HL_*` name is an engine option and
is applied to the container spec, never exported to the guest; everything else
is guest environment. An unrecognised `HL_*` name fails the manifest at load,
and a recognised one the runner cannot yet express (`HL_ULIMITS`) fails its
case by name rather than being dropped.

Check committed golden bytes against the folder's reference emulator, or
replace them explicitly:

```text
cargo run -p testing -- oracle --check core --isa arm64
cargo run -p testing -- oracle --update core --isa arm64
```

Build products and image caches live under `target/testing`; application
folders contain no generated binaries. Removing an application directory
removes its complete definition without changing another test.

`legacy/` is the former monolithic CMake/Python corpus retained temporarily
while its cases are split into independent application folders. It is not the
new test API and receives no new cases.
