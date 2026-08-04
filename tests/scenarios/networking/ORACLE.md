# Networking scenario oracle

The folder-owned YAML is the sole executable definition for all nine
`networking/*` cases. Before deleting the legacy fixture, its inventory was
compared with this file: IDs, images, commands, target selection, classes,
timeouts, and output values matched exactly.

The former scheduler applied its `host_port` fallback to the complete network
task. The folder-owned runner schedules individual cases, so every case now
declares `host_port` explicitly to retain the collision bound. This migration
changes no engine runtime behavior; the retained C engine was not modified or
used as an implementation oracle.

The checkpoint had already removed the legacy registry and generated snapshots.
This closure removes its remaining fixture and unreachable dispatch branch. The
directory-local golden files remain the output authority used by `test.yaml`.
