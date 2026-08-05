# Provider rootfs launch oracle

The retained implementation was inspected before adding the testing provider
surface. The relevant entry points are:

- `/Users/x/dd/engine/src/core/target/aarch64.c::hl_engine_entry` and
  `/Users/x/dd/engine/src/core/target/x86_64.c::hl_engine_entry`: both consume a
  leading `--rootfs DIR`, retain the borrowed argument for the invocation, and
  pass it to `hl_standalone_run`. The target files differ in ISA setup and in
  their surrounding accepted container flags, but root selection precedes ELF
  loading in both.
- `/Users/x/dd/engine/tools/matrix_runner.c::stage_rootfs` creates the private
  per-case tree, stages `/bin/guest` and the target loader/libc when required,
  and returns failure without launching when any bounded path construction or
  filesystem operation fails.
- `matrix_runner.c::open_case_workspace` creates per-run capture/scratch state
  and the typed launch configuration. `run_guest` owns the child process group,
  bounded output, timeout/termination, wait, and cleanup ordering. The parent
  retains the root path until the child has been reaped; `remove_rootfs` removes
  only the files and directories staged by this owner. There is no shared root
  lock or process-global rootfs owner.

The Rust mapping is deliberately split by ownership. `benchmark::Run` owns the
validated provider request and root identity. `benchmark::adapter::Process`
projects that request to a host path for native/QEMU or a guest path plus root
for either engine. `hl_engine::program::PlanSource` owns conversion of the Rust
engine invocation into `RuntimePlan::rootfs`; filesystem lookup and lifetime
then remain owned by the engine execution/source domains.

Native execution uses the executable inside the selected materialized tree.
QEMU receives the same tree as its loader prefix. Both engines receive the same
root and the same absolute guest path. Rooted requests reject relative or
parent-traversing guest paths before process creation. Provider exit, partial
output, timeout, cancellation, and process-tree teardown semantics remain in
the existing `Process::sample` supervisor and are unchanged by this projection.
