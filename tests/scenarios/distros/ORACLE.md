# Distribution scenario oracle

These end-to-end cases preserve the images, commands, target restrictions,
timeouts, resource bounds, and output contracts formerly declared in
`tests/scenarios/fixtures/distros-full.yaml`. All 58 stable IDs have a
one-to-one directory-local definition. Each expected output is now an
owned file under this category instead of an inline string in a shared fixture.

`test.yaml` is the sole declarative registration for this category. The
legacy manifest and its pure Rust loader/runner wrapper were removed only
after matching every ID, image, command, target restriction, class, timeout,
resource declaration, and output value.

This is a representation and ownership migration only. It changes no engine
runtime behavior, so the retained C implementation was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
