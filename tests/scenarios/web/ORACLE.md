# Web scenario oracle

These end-to-end cases preserve all 35 image, command, target, expected-failure,
environment, timeout, and output contracts formerly declared in
`tests/scenarios/fixtures/web-core.yaml`. Expected output is now owned by this
category under `golden/` instead of being embedded in a shared fixture.

The legacy scenario scheduler serialized this entire category through its
`HostPort` fallback when cases declared no resources. Every migrated case
therefore declares `host_port` explicitly so the new repository runner retains
the same scheduling contract.

`test.yaml` is the sole declarative registration for this category. The
legacy manifest and its pure Rust loader/runner wrapper were removed only
after matching all 35 stable IDs and their effective execution and scheduling
metadata.

This is a representation and ownership migration only. It changes no engine
runtime behavior, so the retired C implementation was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
