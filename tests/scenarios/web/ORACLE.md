# Web scenario oracle

These end-to-end cases preserve all 35 image, command, target, expected-failure,
environment, timeout, and output contracts formerly held in the shared web
fixture. The manifest and expected output are owned entirely by this category.

The legacy scenario scheduler serialized this entire category through its
`HostPort` fallback when cases declared no resources. Every migrated case
therefore declares `host_port` explicitly so the new repository runner retains
the same scheduling contract.

This is a representation and ownership migration only. It changes no engine
runtime behavior, so the retired C implementation was not used as an
implementation oracle and `/Users/x/dd/engine` was not modified.
