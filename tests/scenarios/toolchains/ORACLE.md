# Toolchain scenario provenance

The 40 cases in `test.yaml` preserve the stable IDs, exact OCI image
references, class, timeout, target, expected-failure, resource, environment,
compiler invocation, and output substring contracts from
`tests/scenarios/fixtures/toolchains-core.yaml`.

The old manifests generated compiler inputs inside shell heredocs. Those exact
payloads now live in `source/`: two C programs, one C++ program, one Make C
program, two Go programs, two Rust programs, and the original CMake and Make
build descriptions. Installation keeps the original guest paths (`/m.*` and
`/p/*`) and compiler flags. Go's former shell exports are represented by the
typed environment map without changing their values. Each expected substring
is an owned file under `golden/`.

This is a test-definition migration only. It does not claim toolchain execution
success, engine compatibility, or behavioral parity beyond the mechanically
audited scenario contract. No retained C engine source is involved in this
container image/toolchain inventory migration.
