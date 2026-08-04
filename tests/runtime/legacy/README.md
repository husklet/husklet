# Legacy runtime corpus

This directory preserves the pre-migration CMake, Python, TSV, source,
artifact, and report tree. It is opt-in migration material, not the default
runtime test interface.

New and migrated coverage belongs in a self-contained sibling under
`../<name>/`, driven by that folder's `test.yaml`. Delete this directory after
all authoritative cases and oracle evidence have moved; do not add another
central manifest or runner here.
