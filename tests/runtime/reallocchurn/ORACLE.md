# reallocchurn soak oracle

This deterministic prolonged workload was migrated without behavioral changes from `tests/runtime/legacy/oracle/tests/soak/reallocchurn.c`. Its stdout contract is the exact retained golden in `golden/stdout.txt`; exit status zero and byte-for-byte stdout equality are both required. The legacy bounded profile allowed 240 seconds per attempt and one repetition, terminating the complete process group on timeout. The optional typed soak policy preserves those bounds in the ordinary `tests/runtime/<case>/test.yaml` envelope rather than creating a separate soak tree.

The retained inventory classifies this as a Linux-libc stress workload and builds it as a static PIE with optimization, pthread, and math linkage. Both guest ISAs are selected unless the source itself emits architecture-specific instructions. Extended evidence may raise repetitions to ten, but checked-in defaults remain bounded and deterministic.
The legacy inventory records a macOS-host process-group cleanup timeout for this case, while the Linux engine passes it. That host-specific limitation must remain visible in execution evidence; it does not change the active Linux guest contract encoded here.


## Provenance and scope

The source comments describe the stressed mechanism and its deterministic accumulator or completion verdict. This migration only changes test ownership and metadata; it does not claim that one prolonged workload proves the full runtime domain, teardown surface, or performance parity. Runtime-domain acceptance still requires the corresponding retained C-engine audit and focused compatibility evidence.
