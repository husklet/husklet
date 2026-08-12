# Rust executor deletion boundary

The production and packaged engine paths now enter `Program`, which constructs
`runtime::ProductionFactory`; that factory has only the retained-C machine.
`Program` is therefore part of the C product frontend and must not be deleted.

There are no Cargo features or application manifests which opt into Rust guest
execution. `HL_EXECUTION_BACKEND=rust` is rejected by the production factory.
The former `rust-execution` feature, `RustRuntimeFactory`, `RustRuntimeMachine`,
their unit fixtures, and the old Rust `Program` guest fixture are removed.

## Remaining source-only closure

The Rust guest executor is still physically compiled under
`src/ffi/linux/execution/**`, with reexports through `src/ffi.rs`,
`src/ffi/linux.rs`, and `src/native/mod.rs`. Nothing constructs it in production.
The live checkpoint network service has moved to `ffi/linux/network/**`, and
`runtime/api.rs` now reexports it from that non-executor location. The service
still shares `execution::process_memory` and the file-transfer registry while
the old syscall router is compiled; those adapters must move before deleting
the directory wholesale.

The bounded follow-up is:

1. Move the shared process-memory and file-transfer adapters out of the
   executor directory without narrowing SCM_RIGHTS/checkpoint behavior.
2. Remove the temporary `GuestExecutionPort` trait and its C and Rust impls;
   the C worker already invokes `CGuestExecutor` directly.
3. Delete `ffi/linux/execution/**`, its three `GuestExecutor` reexports, and
   Rust-native executor-only helpers proven caller-free after step 2.
4. Compile every consumer, then mutate the deleted exports to confirm the
   C-only gate is non-vacuous.

Do not remove `program.rs`, loader inspection, container lifecycle, or the
`hl-runtime`, `hl-execution`, `hl-memory`, and `hl-task` crates with this closure.
The retained C adapter independently uses typed CPU snapshots, runtime syscall
trap/task state, and loader services.
