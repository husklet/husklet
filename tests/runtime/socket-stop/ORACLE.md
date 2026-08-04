# Blocked socket teardown oracle audit

This category is a lifecycle integration test, not a standalone output test.
`main.c` creates an `AF_UNIX` stream socket pair, leaves both endpoints open,
and blocks while reading one endpoint because nothing writes to its peer. The
empty golden file is therefore not evidence of successful completion. The
required behavior is for an external controller to force-stop and reap the
blocked engine without affecting another engine.

## Retained C implementation studied

- `../engine/src/linux_abi/number.c` maps the x86-64 `read` and `socketpair`
  syscall numbers to the canonical AArch64 numbers 63 and 199.
- `../engine/src/linux_abi/syscall/dispatch.c::service` and `service_local`
  route those canonical operations to their syscall-family owners.
- `../engine/src/linux_abi/syscall/net.c::svc_net`, case 199, validates the
  socket type, creates the host pair, applies descriptor flags and no-SIGPIPE
  behavior, copies the descriptor pair to guest memory, and registers the Unix
  stream endpoints. Error paths close both host descriptors before returning.
- `../engine/src/linux_abi/syscall/io.c::svc_io`, case 63, validates the guest
  descriptor and calls `guest_fd_read`. A potentially blocking read publishes
  sleeping state with `ts_wait_enter`, restores running state with
  `ts_wait_leave`, and retries `EINTR` only through `SVC_EINTR_RESTART`.
- `../engine/src/linux_abi/syscall/signal.c::syscall_should_restart` owns the
  pending-signal, checkpoint, exit-state, and `SA_RESTART` decision used by
  blocking syscalls. This fixture does not install a signal handler, so it does
  not establish cooperative `EINTR` or `SA_RESTART` behavior.
- `../engine/include/hl/activation.h` and
  `../engine/src/core/activation.c::hl_activation_kill`,
  `hl_activation_wait`, and `hl_activation_process_destroy` own external
  termination and reaping. POSIX force-stop kills the process group and then
  terminates the activation domain so descendants that changed groups are not
  left alive; waiting reports signal termination.
- `../engine/src/core/engine.c::hl_engine_request` handles
  `HL_ENGINE_REQUEST_FORCE_STOP`, including a request made before the host
  process exists. `hl_engine_destroy` force-terminates and waits for a running
  process during teardown.

The socket endpoints are owned through descriptor/open-file-description and
socket identity state until process teardown closes them. The blocked read does
not transfer that ownership. Stop state belongs to one engine activation; it is
not process-global, and stopping one engine must not terminate an unrelated
engine.

## Rust ownership

- Unix socket-pair identity and endpoint lifetime belong to `hl-network` and
  `hl-descriptor`, joined by runtime adapters.
- Blocking syscall state, restart decisions, and cancellation observation
  belong at the runtime/execution boundary.
- Force-stop state, host process-tree termination, reaping, and workspace
  cleanup belong to the `hl-engine` launcher and container composition.

The retained Rust integration test in
`../engine_rust/src/app/hl-engine/tests/compat.rs` supplies the missing
orchestration: it starts the fixture, requests force-stop, expects signal 9,
checks workspace removal, and separately verifies that a second engine remains
alive. That is migration evidence, not current folder-runner evidence.

## Current evidence and limits

Direct QEMU execution is expected to time out because QEMU supplies no external
stop request. The category remains typed `!broken` until the unified runner can
start a case, request force-stop, wait for signal termination, verify cleanup,
and run the two-engine isolation check.

This fixture does not test data transfer, readiness, peer-close EOF, graceful
interrupts, `EINTR`, `SA_RESTART`, checkpoint cancellation, or ordinary socket
shutdown. It proves only blocked socket teardown once the external lifecycle
orchestration is restored.
