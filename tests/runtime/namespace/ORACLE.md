# Namespace probe oracle

This workload preserves `tests/runtime/legacy/source/namespace.c` and its exact
`namespace-ok\n` golden as one independently removable runtime test.

## Retained C audit

Read-only files and entry points studied:

- `../engine/src/linux_abi/syscall/proc.c`: syscall cases 97 (`unshare`) and
  268 (`setns`).
- `../engine/src/linux_abi/container/vfs.c`: `synth_misc_dir_open` for
  `/proc/<pid>/ns`, `ns_link_target`, and the procfs namespace open/stat paths.
- `../engine/src/core/provider/namespace.c`: `hl_provider_namespace_install`,
  `hl_provider_namespace_resolve`, `hl_provider_namespace_revoke`, and launch
  namespace lookup/iteration.

The retained syscall path has no allocated namespace object, lock, or teardown:
`unshare(0)` and recognized flags return success, unknown bits return `EINVAL`,
and `setns` only rejects a negative descriptor before otherwise returning success.
That means its valid-descriptor `setns` behavior is incomplete relative to Linux
and this workload's native oracle. The VFS side separately synthesizes one stable
namespace set for a container. It enumerates the ten procfs namespace links,
reads a well-formed host `/proc/self/ns/<kind>` link when available, and otherwise
uses stable initial-namespace inode fallbacks. The provider namespace is a bounded
generation-qualified table installed transactionally: it validates all nodes and
conflicts in a pending allocation, swaps only on success, and revocation clears
all nodes while advancing a nonzero generation. Its process-global launch table
has no internal lock; ownership and serialization belong to launch lifecycle.

There are no guest-ISA branches in these namespace owners. Syscall-number ABI is
selected by the translator. `ns_link_target` is host-sensitive: Linux uses host
nsfs identity; macOS and hosts without a valid link use fixed well-formed values.

## Capability matrix

| Capability | Retained C | Rust owner | Status |
|---|---|---|---|
| syscall routing and ISA numbers | translator + `proc.c` | `hl-linux/src/syscall/table.rs` | implemented |
| unknown `unshare` bits -> `EINVAL` | `proc.c` | `hl-runtime/src/process/namespace.rs` | implemented, exercised |
| `unshare(0)` | `proc.c` | `hl-runtime/src/process/namespace.rs` | implemented, exercised |
| unsupported user namespace -> `ENOSYS` | C returns success | `hl-runtime/src/process/namespace.rs` | C and QEMU-host policy diverge; Rust exercised |
| negative `setns` fd -> `EBADF` | `proc.c` | `hl-runtime/src/process/namespace.rs` | implemented, exercised |
| namespace type must match handle | missing | `NamespaceHandleRegistry` + process namespace adapter | Rust implemented, exercised |
| UTS permission check | missing | process namespace adapter + `hl-task` credentials | Rust implemented, exercised |
| UTS identity allocation/join | missing | `hl-task/src/registry/namespace.rs` | implemented, not successful-path exercised here |
| `/proc/self/ns/uts` stable identity | synthetic host/fallback link | `hl-vfs` procfs + `hl-runtime` procfs adapter | implemented, open exercised |
| provider namespace transactional install/revoke | `core/provider/namespace.c` | `hl-provider` namespace | separate provider capability, not exercised |

The test deliberately probes only validation, permissions, procfs handle opening,
and close. It does not claim successful namespace creation/join, namespace
inheritance, hostname isolation, fork/exec behavior, checkpoint, or concurrency.

## Oracle limitation

The checked golden has valid provenance from the legacy Husklet workload, but
QEMU user mode is not an authoritative oracle for its user-namespace admission
policy. On this host both `qemu-aarch64` and `qemu-x86_64` reach assertion 8:
`unshare(CLONE_NEWUSER)` does not return Husklet's deliberately unadvertised
`ENOSYS`. The Rust engine returns the preserved `ENOSYS` on both guest ISAs.
The QEMU mismatch is retained as evidence; it must not be hidden by accepting an
additional errno or rewriting the golden.
Consequently `test.yaml` intentionally omits an `oracle` command: the unified
runner executes the checked Rust-engine contract without presenting QEMU's host
policy as C-oracle parity.
