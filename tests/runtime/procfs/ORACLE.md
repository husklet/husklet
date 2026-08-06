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

Descriptor enumeration and targeted lookup preserve separate retained
capabilities. `proc_fdvis_list` and `proc_fd_dir_pid_open` take a bounded,
ordered view of the published arena for an fd/fdinfo directory, while
`proc_fdvis_lookup` and `proc_fd_link_pid` select one live descriptor identity.
Rust maps listing to `DescriptorTable::bounded_active_snapshots`: count and peak
vector bytes are admitted atomically under the table read lock before allocation,
then only descriptor numbers cross the VFS Source boundary. Targeted fd links,
fdinfo, kind, and metadata use `DescriptorTable::snapshot(number)` followed by
an exact pin and descriptor-generation/full `DescriptionIdentity { identity,
generation }` comparison; close and
same-number reuse therefore cannot substitute the replacement. Snapshot budget
failure is the typed `ProcfsError::ResourceLimit`. This stage retains the current
eager directory snapshot timing; post-publication lazy capture remains the next
descriptor-directory lifetime slice. The peer-fd shared-path identity gap is
unchanged.

The syscall-side readlink route was additionally audited in read-only
`../engine/src/linux_abi/syscall/fs.c` at `svc_fs`'s `readlinkat` case and its
calls to `procfd_num`, `proc_any_leaf`, `proc_pid_member`, and
`proc_fd_link_pid`. Self links reject engine-only eventfd peers, translate PTY
master/slave handles to `/dev/ptmx` or `/dev/pts/N`, then use the native fd path;
peer links validate live process membership and query the peer host fd table.
Closed, missing, pathless, or failed host lookups produce `ENOENT`; successful
targets use Linux readlink truncation to the guest buffer length without a
terminator. There is no guest-ISA branch; host-specific path discovery and peer
inspection remain behind native adapters.

The retained directory materialization was audited at `g_procfd_dirs`,
`procfd_dirs_reap`, `procfd_dirs_atexit`, `proc_fdinfo_dir_open`,
`proc_fdinfo_text`, `proc_fd_dir_pid_open`, `proc_fd_link_pid`, and `proc_open`
in `../engine/src/linux_abi/container/vfs.c`. Each bounded global slot owns a
temporary directory path and returned host directory fd; opportunistic reap
removes it after that fd closes, while atexit force-removes survivors. Directory
names are materialized from the open-fd set at directory open. Opening or reading
an `fdinfo/N` body instead rechecks the then-live descriptor and renders current
position, flags, mount, and type-specific state. The retained ordering is thus
name snapshot at directory-open followed by live body admission/rendering, not
one atomic arena snapshot spanning both; a listed name may disappear before its
body is opened.

### PID reuse identity prerequisite

The retained PID/start-token path was audited in
`../engine/src/linux_abi/container/vfs.c` at `launch_reg_publish`,
`proc_reg_write_files`, `proc_reg_publish`, `proc_reg_after_fork`,
`proc_pid_member`, `proc_reg_mark_child`, and `proc_reg_reap`. A registration is
owned by the live host process and container process domain. Publication writes
the numeric host PID plus a sibling `b<pid>` record containing
`hl_host_process_info.start_time_ns`; fork publishes the child marker and birth
token before the parent returns, exec atomically replaces presentation data,
and exit/reap unlinks every record before the host PID can be reused. Registry
file mutation is bounded and serialized by filesystem create/store/rename;
there is no blocking, cancellation, partial guest result, guest-ISA branch, or
Linux errno conversion in this identity publication path. Host differences are
contained by `hl_host_process_read`.

Rust owns the equivalent lifetime in `hl_task::TaskRegistry`: `ProcessId`
combines a slot and generation, allocation increments the generation, and
`process_snapshot` rejects a stale tuple under the registry lock. Procfs now
owns the pointer-free mirror `ProcessIdentity { slot, generation }` and resolves
a numeric path to that exact live identity in one registry lookup. Unpublished
`Starting` processes remain absent, while a running process undergoing staged
exec remains visible through its old published identity until exec publication,
matching the retained temp-file rename behavior. Existing numeric procfs
consumers remain unchanged in this prerequisite; the next consumer migration
must accept and validate `ProcessIdentity` directly and must not re-resolve its
PID, which would silently retarget an operation after slot reuse.

That consumer migration is now complete for process identity. Every per-process
`hl-vfs::procfs::Source` method receives `ProcessIdentity`; `open`, `read_link`,
`kind`, `metadata`, `uts_namespace`, and `namespace_inode` resolve a numeric PID
once and carry the resulting tuple through thread validation and source access.
Dynamic `comm` and `oom_score_adj` open descriptions retain that tuple, so a
reaped slot cannot retarget an already-open description after reuse. The
`TaskProcfs` adapter converts the tuple directly to `ProcessId` and uses exact
registry snapshot APIs rather than scanning numeric process snapshots.

Namespace opens synthesize their OFD metadata from that already resolved tuple;
they never call the public path-metadata operation and therefore cannot perform a
second numeric lookup after PID reuse. UTS and network inode identities come from
the tuple-scoped source views, while static namespace inodes are exposed only
after the same tuple passes an exact process lookup. Cgroup membership similarly
validates the pinned tuple immediately before consulting the immutable
`CgroupView`. Its numeric PID is only an index into that snapshot and the rendered
unified membership value (`0::/`) contains no process-generation state.

The retained thread implementation was audited directly in
`../engine/src/linux_abi/thread.c`: `thread_register`, `thread_unregister`,
`thread_tid_alive`, `thread_tid_list`, `thread_live_count`, `thread_after_fork`,
and `cpu_tid` own numeric live-thread membership under `g_threg_m` from
registration through exit. The retained procfs consumers in
`../engine/src/linux_abi/container/vfs.c` (`proc_task_tid_visible`,
`proc_task_dir_open`, `proc_dir_try_open`, `proc_deself`, and `proc_open`)
snapshot numeric directory names and revalidate numeric membership on later
path access. They have no generation identity, fold task leaves onto process
leaves, and expose only peer leaders; a reused numeric TID can therefore name a
different retained thread on a later lookup.

Rust maps registry membership to `hl_task::ThreadId(slot, generation)` and the
VFS boundary to `ThreadIdentity`. `TaskProcfs::resolve_thread` resolves a TID
within an already exact `ProcessIdentity` from one registry snapshot. Explicit
task paths use that lookup as their validation linearization point before a
process-owned projection; directory enumeration remains a numeric snapshot and
does not promise that a later open succeeds. Dynamic `comm` OFDs retain both
exact identities, so exit and same-number reuse retire old reads, metadata, and
writes without mutating the replacement. Process-level `comm` resolves the
exact leader identity. After process exit the live thread entry is absent, but
the zombie `ProcessSnapshot` owns the leader identity and name until reap; the
shared leader slot cannot be reused while that process slot remains occupied.
Thus zombie process-level comm remains readable, while an explicit zombie task
path is absent and reap retires the pinned process identity. Rust intentionally
provides stronger peer-thread identity than the retained numeric peer-leader
projection.

### Magic-link open lifetime

The `/proc/[self|pid]/{root,cwd}` open and lifetime path was audited directly in
the read-only retained engine. `../engine/src/linux_abi/syscall/fs.c` handles
`openat` in `svc_fs`; unlike its dedicated `exe` and `map_files` open branches,
root/cwd reach the ordinary confined `open`/`openat` route after `proc_open`
declines them. Their readlink and stat branches use `proc_self_leaf`, the live
`g_cwd` or `/` target, and `xresolve_overlay`. A successful retained open has
already produced a host descriptor before the guest descriptor is returned.
The same file's close case calls `fd_reset_emul` before the host `close`, so the
host descriptor owns the followed directory until final descriptor teardown.
`chdir` and `fchdir` update process-local `g_cwd` only after a successful host
directory change. The filesystem context is detached on fork unless `CLONE_FS`
shares it; root/cwd link synthesis has no guest-ISA branch, while host path
resolution is selected by the retained POSIX adapters. There is no partial
result, blocking, cancellation, or wakeup path in this open; lookup errors are
returned before descriptor publication. Rust's explicit typed root/cwd redirect
therefore has no one-to-one retained helper; it replaces the retained route's
dependence on the materialized proc tree while preserving its committed-host-fd
lifetime.

Rust maps live root/cwd state to `hl-runtime::WorkingDirectory` and
`hl-runtime::TaskProcfs`, magic-link identification and metadata to
`hl-vfs::Procfs`, and the cross-domain follow to `NativePath::procfs_plan` plus
the ordinary resolver. `PendingOpen` pins the resolved parent during the
transaction, installs the host file only in `PreparedPathOpen::commit`, and
publishes that file through an `Arc<NativeFile>` open description. Consequently
the prepared object is deliberately not an open file before commit, rollback
has no leaked host file, and the committed open description remains valid after
the procfs task is reaped. The focused engine tests cover both transaction
ordering and post-reap open-description lifetime; they do not claim peer
magic-link open support.

| Retained C capability | Rust owner | Mapping |
|---|---|---|
| live root/cwd target selection | `WorkingDirectory` and `TaskProcfs` | process-scoped snapshot used by magic-link follow |
| confined target resolution | `NativePath::procfs_plan`, `hl-vfs::Resolver`, `PendingOpen` | guest target resolved under the selected root with a pinned parent |
| atomic open before fd publication | `PreparedPathOpen::commit`, then descriptor install publication | object becomes usable only after commit succeeds |
| OFD lifetime independent of proc task | `Arc<NativeFile>` containing the committed host file | task reap removes proc visibility without retiring an open description |
| final close teardown | `NativeFile` and descriptor-table last-reference teardown | host file and path leases are released by their owners |

## Canonical ownership

The retained manifest contains 57 cases. The canonical inventory contains 112
rows for 56 cases across AArch64 and x86-64; the build plan/report add both
`peer-fd` rows for 114 planned rows total. The YAML preserves all 57 IDs and
both `peer-fd` targets, leaving that case visibly broken rather than silently
dropping it. The external-service image acceptance inventory is preserved in
`images.tsv` with its three byte-exact group/capability goldens; those assets
remain evidence and are not misrepresented as ordinary QEMU rows.
