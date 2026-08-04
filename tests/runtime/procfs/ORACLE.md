# Procfs retained-C audit

This folder migrates the complete procfs compatibility cohort: the 56 cases
registered by the canonical inventory for two guest ISAs, plus `peer-fd`, whose
known descriptor-identity defect kept it out of that inventory. Sources,
expected bytes, compiler flags, targets, and launch environment are preserved.

## Retained implementation studied

The read-only oracle was `../engine/src/linux_abi/container/vfs.c`. The audit
covered initialization and descriptor visibility (`proc_fdvis_reserve`,
`proc_fdvis_publish`, `proc_fdvis_lookup`, `proc_fdvis_list`,
`proc_fdvis_fork_prepare`, `proc_fdvis_after_fork`, and cleanup), text and map
rendering (`proc_text_fd`, `proc_maps_fd`, `proc_status_text`, `proc_stat_text`,
`proc_environ_text`, `proc_limits_text`), process registration and peer lookup
(`proc_reg_publish`, `proc_reg_after_fork`, `proc_reg_reap`,
`proc_pid_member`), directory and link synthesis (`proc_fd_dir_pid_open`,
`proc_fd_link_pid`, `proc_task_dir_open`, `proc_dir_try_open`,
`proc_root_dir_open`), and the `proc_open` dispatch. Loader publication was
also traced through `../engine/src/linux_abi/elf.c` and
`../engine/src/linux_abi/x86.c`; sentry ownership and local per-process leaves
were traced through `../engine/src/linux_abi/sentry.c` at
`sentry_proc_fork`, `sentry_proc_release`, `sentry_proc_exec_sweep`, and
`sentry_worker_proc_leaf`.

The C engine owns procfs state in process-global tables. Descriptor visibility
uses a bounded shared arena with reservation/publish/cancel transitions and an
explicit fork plan. Process registrations are published after exec/fork and
removed on reap/exit. Generated text is copied into bounded synthetic read-only
descriptors. Table mutation is serialized by the arena operations; host calls
are performed after identities are resolved. Open/readlink preserve normal
Linux lookup errors, read-only leaves reject write intent, and descriptor and
process teardown remove visibility before identity reuse. AArch64 and x86-64
publish different ELF auxv and CPU feature models; Linux reads host proc
identity directly while macOS uses a synthetic subset.

## Rust ownership mapping

`src/runtime/hl-vfs/src/procfs/{mod.rs,model.rs,file.rs,mount.rs}` owns typed
procfs nodes, bounded value snapshots, rendering, links, metadata, mounts,
network views, CPU topology, limits, address spaces, and descriptor views.
`src/containers/hl-engine/src/ffi/linux/execution/process_resources.rs` owns
instance-local descriptor and working-directory publication, while
`process_memory.rs` and execution routing publish address-space and lifecycle
snapshots. Loader and fork composition publish and reap those views without
moving Linux policy into the native execution boundary.

The retained descriptor arena corresponds to Rust descriptor snapshots and
resource catalogs; retained registration tables correspond to task/resource
publication; retained renderers correspond to `Procfs::open`, `read_link`,
`kind`, `metadata`, and the typed model renderers. Known gaps remain explicit:
`peer-fd` lacks shared cross-process path identity; `thread-self` and
`self-comm` diverge from Linux live task/name semantics. `fork-self`,
`self-vm`, `nslinks`, and `peer-identity` are unsupported on the macOS backend
but remain enforceable on Linux.

## Canonical ownership

The retained manifest contains 57 cases. The canonical inventory contains 112
rows for 56 cases across AArch64 and x86-64; the build plan/report add both
`peer-fd` rows for 114 planned rows total. The YAML preserves all 57 IDs and
both `peer-fd` targets, leaving that case visibly broken rather than silently
dropping it. The external-service image acceptance inventory is preserved in
`images.tsv` with its three byte-exact group/capability goldens; those assets
remain evidence and are not misrepresented as ordinary QEMU rows.
