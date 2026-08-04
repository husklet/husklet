# Networking scenario oracle

These end-to-end cases preserve the images, guest commands, target selection,
timeouts, and output contracts from
`tests/scenarios/fixtures/networking-core.yaml`.

The former scheduler applied its `host_port` fallback to the complete network
task. The folder-owned runner schedules individual cases, so every case now
declares `host_port` explicitly to retain the collision bound. This migration
changes no engine runtime behavior; the retained C engine was not modified or
used as an implementation oracle.
