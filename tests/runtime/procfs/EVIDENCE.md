# Procfs provider evidence

On 2026-08-03 a bounded 18-worker sweep loaded `test.yaml` and, for each of the
112 rows in the canonical inventory, invoked the declared cross-compiler with
the case flags, verified the output as a static ELF for the requested machine,
and ran it under `qemu-aarch64` or `qemu-x86_64` with a 30-second row timeout.
Each exit status and byte comparison against the declared golden is recorded in
`EVIDENCE.tsv`; `PASS` means exit and stdout both matched, while `FAIL` means
the guest exited successfully but its host-derived procfs bytes differed.

The manifest also retains `peer-fd` for both ISAs; those two known-broken rows
appear in the build plan but not the 112-row provider inventory and were not
silently counted as provider passes. All 112 inventory-row compilations
succeeded and all 112 QEMU processes exited zero. Eighty-two rows matched
byte-for-byte and 30 exposed QEMU host-view differences. These
provider observations are deliberately separate from YAML engine status: QEMU
cannot establish Husklet's configured procfs, cgroup, namespace, CPU, or volume
policy, and a provider difference does not make an engine case broken. The
typed `broken` and `unsupported` states in `test.yaml` therefore retain only
their independently audited engine or host-support evidence.

The production command
`HL_COMPAT_JOBS=18 target/debug/testing oracle procfs --check` loaded the
category and began the same declared-target checks, then failed fast at the
first expected provider byte difference. No QEMU descendants or new zombies
remained after the bounded sweep.
