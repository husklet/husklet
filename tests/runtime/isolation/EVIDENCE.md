# Isolation migration evidence

On 2026-08-03 the integrated typed-native runner was executed twice from the
shared tree: first with 18 workers and then serially with one worker. Both runs
emitted valid `hl-native` and `hl-native-detail` activation diagnostics for
both guest ISAs, but the runner process terminated with
`*** stack smashing detected ***` before it could publish a durable per-row
result ledger. The serial reproduction excludes worker concurrency as the
cause. The evidence locates the failure only at the generic native/FFI
activation or teardown boundary before durable row attribution; it does not
identify an isolation policy function. Consequently all migrated rows are
explicitly broken; none is promoted from partial diagnostic output. Direct
QEMU execution is not authoritative for CPU, cgroup, procfs, rlimit, or
namespace policy because those views belong to the host rather than Husklet's
configured guest engine.

All 21 stable IDs, target pairs, compiler flags, expected bytes, and the seven
configured cases were mechanically compared with the retired
manifest and `fixture-schema.tsv`. The environment contracts are `HL_CPUS=2`
for cgroup CPU, default CPU, and sysfs CPU cases; `HL_CPUS=1/2` for the two CPU
cap cases; `HL_MEM_MAX=536870912`; and
`HL_ULIMITS=nofile=1024:2048`.

## Migration-boundary verification

On 2026-08-04 the self-contained folder was compared directly with the tracked
pre-migration isolation manifest at `HEAD`. The legacy and YAML case sets are
identical: 21 unique IDs, no omission and no addition. All 18 renamed C source
files and all 21 primary golden files compare byte-for-byte with their legacy
counterparts. The four image goldens also compare byte-for-byte and are now
owned only by `golden/images`; `images.tsv` preserves all four external-service
registrations and has no legacy-path reference.

The complete build matrix was then compiled with 18 workers using the flags in
`test.yaml`: 21 cases for each of arm64 and amd64. All 42 compilations produced
the requested statically linked PIE for the declared machine. The existing
`target/debug/testing runtime isolation --isa arm64` runner loaded the complete
definition and reported each of the 21 typed `BROKEN` rows before returning the
expected `no active runtime cases` error; this proves discovery and schema
loading but is deliberately not counted as engine execution.

An isolated rebuild of the Rust definition tests was attempted, but concurrent
repository builds filled `/tmp` and it stopped with `ENOSPC` before the test
binary linked. The lane's 1.3 GiB partial target and 33 MiB C-build directory
were removed. This is a disk-capacity blocker for that duplicate build, not a
test verdict; the 42-row C build and existing-runner definition load are the
available focused evidence until the boundary is committed and verified from a
clean checkout.
