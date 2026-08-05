# Vectored descriptor output oracle

This audit covers the descriptor behavior required when an image replaces
itself with `execve` and the new program writes through an inherited standard
stream with `writev`.

## Retained C implementation studied

- `/Users/x/dd/engine/src/linux_abi/syscall/io.c`: `service_io` cases 64
  (`write`), 66 (`writev`), 23 (`dup`), and 24 (`dup2`/`dup3`), plus
  `fd_carry_virt`. Case 66 admits the guest vector, executes one operation on
  the same host descriptor/OFD as scalar write, preserves partial results,
  retries only under the selected `SA_RESTART` policy, and queues `SIGPIPE`
  after `EPIPE`.
- `/Users/x/dd/engine/src/linux_abi/linux_abi.c`:
  `hl_linux_fd_close`, `hl_linux_fd_exec`, and `hl_linux_fd_exec_all`. The fd
  table owns descriptor flags and generations; the OFD table owns reference
  count, active-operation count, status, offset, object, and I/O mutex. Exec
  closes only `FD_CLOEXEC` entries. A non-CLOEXEC fd continues to reference the
  same OFD and is finalized only after its last reference and active operation.
- `/Users/x/dd/engine/src/linux_abi/fork.c`:
  `hl_forkserver_runner`. Launch duplicates the client's streams onto 0, 1,
  and 2 before closing surplus received descriptors, so stdout identity
  survives an external-program exec.
- `/Users/x/dd/engine/src/core/activation.c`: activation spawn construction.
  Linux and Darwin use distinct descriptor-isolation mechanisms, but both
  explicitly retain or duplicate standard streams before closing unrelated
  descriptors.

The syscall numbers and guest marshalling differ between AArch64 and x86-64,
but descriptor/OFD ownership and vectored-write behavior do not. Host-specific
spawn isolation also does not change the guest-visible contract.

## Rust ownership and gap

- `hl-descriptor::DescriptorTable` owns descriptor number, generation, and
  close-on-exec state. `OpenFileDescription` and `OperationLease` own shared
  OFD identity and pin one operation against teardown.
- `hl-runtime::descriptor::Exec` forks the descriptor table and removes only
  close-on-exec descriptors before publishing an image.
- `hl-runtime::filesystem::RuntimeFilesystemSyscalls::execute_vector` owns
  iovec admission, partial-fault behavior, cancellation, and errno mapping.
- `hl-engine::ffi::linux::execution::descriptor::StandardIo` adapts injected
  stdin/stdout/stderr streams, including daemon log capture.

Before this change, an OFD implementing scalar `write` but not the optional
`write_vector_context` method returned `NotSupported` for `writev`. External
`grep` uses `writev` for a successful match, so the guest exited zero while its
stdout was discarded with `ENOSYS`. The retained engine forwards both scalar
and vectored writes to the same inherited descriptor.

The generic default now performs one legal partial `writev` result by applying
the scalar contextual write to the first nonempty segment. Domain-specific
OFDs remain free to override it for gather atomicity, pipe `PIPE_BUF`
semantics, positional access, or native vector acceleration. Empty vectors
return zero. Descriptor identity, exec survival, cancellation, errno, locking,
and teardown remain owned by their existing layers.
