# Rust executor deletion boundary

The production and packaged engine paths now enter `Program`, which constructs
`runtime::ProductionFactory`; that factory has only the retained-C machine.
`Program` is therefore part of the C product frontend and must not be deleted.

There are no Cargo features or application manifests which opt into Rust guest
execution. `HL_EXECUTION_BACKEND=rust` is rejected by the production factory.
The former `rust-execution` feature, `RustRuntimeFactory`, `RustRuntimeMachine`,
their unit fixtures, and the old Rust `Program` guest fixture are removed.

## Deleted closure

The Rust guest executor, scheduler, syscall router, native translator adapter,
temporary `GuestExecutionPort`, and their tests have been deleted. The live
checkpoint network service is under `ffi/linux/network/**`; SCM_RIGHTS file
transfer is under `ffi/linux/file_transfer/**`. Neither depends on the deleted
executor tree.

Do not remove `program.rs`, loader inspection, container lifecycle, or the
remaining `hl-runtime`, `hl-memory`, and `hl-task` crates with this closure.
The former `hl-execution` crate was part of the deleted executor and no longer
exists. The retained C adapter is now under `execution/`; it still uses Rust
runtime, memory, task, loader, provider, and worker-supervision services. Those
host packages are not a second guest execution engine.

The production workers are built wholly from this repository. The embedded C
source root is `src/runtime/native`; `../engine` is a read-only oracle and
`../engine_rust` is migration evidence, not a Cargo, CMake, link, or runtime
dependency.
