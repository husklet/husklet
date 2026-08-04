# Filesystem scenario oracle

These end-to-end cases preserve the images, commands, timeouts, and output
contracts from `tests/scenarios/fixtures/filesystem-core.yaml`. Expected output
is now owned by this category instead of embedded in a shared fixture.

This migration only changes test ownership and representation. It changes no
runtime implementation, so the retained C engine was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
