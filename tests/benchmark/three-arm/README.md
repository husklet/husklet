# Three-arm benchmark campaign staging

`generate.sh` creates the first content-equivalent workload for a future strict
three-arm campaign: one Linux x86-64 ELF for Husklet and OrbStack/Docker, and one
x86-64 Mach-O built from the same source for host Rosetta. It copies the exact
macOS `arch` and Docker client executables into the machine-local output, invokes
those copies through `/usr/local/bin/mac`, verifies the pinned image ID, and
requires canonical exact-output parity.

Run it with a new absolute directory beneath the repository workspace:

```text
tests/benchmark/three-arm/generate.sh /Users/.../husklet/target/three-arm-<unique>
```

The generator deliberately does not emit campaign YAML until every workload the
strict schema requires exists. `BLOCKERS.txt` records what remains. Generated
binaries, copied host tools, hashes, and output are machine-local and belong
under `target/`; they must not be committed.

If `testing` is not at `target/debug/testing`, set `THREE_ARM_TESTING`. For a
dynamically linked development build, also set `THREE_ARM_TESTING_LIBRARY_PATH`
to the directory containing `libhl_native_engine.so`.
