# Distribution scenario oracle

These end-to-end cases preserve the images, commands, target restrictions,
timeouts, resource bounds, and output contracts from
`tests/scenarios/fixtures/distros-full.yaml`. Each expected output is now an
owned file under this category instead of an inline string in a shared fixture.

This is a representation and ownership migration only. It changes no engine
runtime behavior, so the retained C implementation was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
