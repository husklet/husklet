# Procfs compatibility gap matrix

Evidence source: `FULL_CORPUS_001.tsv`, the retained C fixtures under
`/Users/x/dd/engine/tests/compat/procfs`, and the retained implementation in
`/Users/x/dd/engine/src/linux_abi/container/vfs.c`. The Rust surface inspected is
`hl-vfs::procfs` plus `hl-runtime::TaskProcfs`.

The report contains 112 ISA-expanded procfs rows: 16 pass and 96 fail. Most
failures have the same verdict on both ISAs, so they cluster at shared runtime
boundaries rather than instruction lowering.

| Fixtures | C/Linux semantic source | Rust gap | Owning invariant |
| --- | --- | --- | --- |
| `pf-selflink`, part of `pf-forkself` | `/proc/self` is a per-caller magic link whose decimal target changes after fork | Paths below `self` were normalized, but the link itself did not exist | Resolve magic links from the calling task identity at lookup time, never from cached launch identity |
| `pf-threadself`, `pf-selftask` | `/proc/thread-self -> <tgid>/task/<tid>` and `/proc/<pid>/task` enumerate live threads | Procfs receives only a process number and has no thread view | Carry caller process and thread identity through the VFS lookup context; enumerate the task registry snapshot |
| `pf-selfstat`, `pf-forkself`, `pf-procstate`, `pf-futexstate` | `stat`, `status`, and peer views expose live PID/PPID/state/thread values | `status` exists; `stat` is absent and sleeping/waiting state is collapsed into `Running` | One coherent task snapshot must expose Linux lifecycle and scheduler state to every process leaf |
| `pf-selfcomm`, `pf-comm-status`, `pf-peer-identity` | `comm`, `status:Name`, and `stat` field 2 share mutable task names, including `prctl` updates | Name exists only in the status projection; no `comm`/`stat` node | Task name is registry-owned state shared by every renderer, not independent procfs strings |
| `pf-selfcmdline`, `pf-selfenviron`, `pf-peer-identity` | NUL-delimited exec argv/environment, replaced atomically on exec and visible to peers | No exec image metadata in `ProcessView` | Publish immutable exec-image identity on successful exec and snapshot it with task state |
| `pf-selfexe`, `pf-selfcwd`, `pf-selflink` root check | Magic links reflect canonical executable, working directory, and namespace root | Namespace root is now an explicit path-host identity and `/proc/<pid>/root` is live; cwd remains outside the typed procfs model (exe currently passes through another route) | Resolve process path identities through explicit runtime ports with caller/target namespace context |
| `pf-maps`, `pf-mapnames`, `pf-selfsmaps`, `pf-selfstatm`, `pf-selfrss`, `pf-selfvm` | Mapping rows, permissions, backing names, RSS and size derive from the live address-space ledger | Procfs has no mapping source or memory counters | The mapping coordinator must provide one read-only coherent process-memory snapshot |
| `pf-selffd`, `pf-selffd-link`, `pf-self-fd-links`, `pf-peer-fd` | FD directory, fdinfo, and links use live descriptor/OFD identity and work for peer processes | Current-process descriptor snapshots exist; `TaskProcfs::descriptors` rejects peer processes | Descriptor tables must be addressable by target process identity with OFD lifetime preserved |
| `pf-selflimits`, `pf-selfcaps`, `pf-umask`, `pf-permbits` | Limits, credentials, caps, umask and ownership come from one live task/fs snapshot | Limits/caps/status exist; umask and complete metadata permissions do not | Extend the typed task/fs view rather than hardcoding leaf output |
| `pf-nslinks`, `pf-peer-ns` | Namespace links carry stable type-specific inode identity shared by members | UTS data exists, but `/proc/<pid>/ns/*` links are absent | Expose every namespace identity from `TaskRegistry`, preserving shared identity across fork/setns |
| `pf-selfmounts`, `pf-mountinfo-bind` | `mounts`, `mountinfo`, and `mountstats` describe the process mount namespace and projected binds | No mount-namespace view | VFS/mount ownership must publish a typed mount snapshot scoped to the target process |
| `pf-selfcgroup`, `pf-cgroup-ro` | Proc and cgroupfs agree on membership and controller files; writes obey permissions | Only CPU-set files are projected | A cgroup domain must own membership, controller values, and writable policy; procfs is a renderer |
| `pf-net`, `pf-net2`, `pf-net-iso`, `pf-netnone-direct` | `/proc/net` and `/proc/self/net` render the caller's network namespace | No network namespace source in procfs | Network runtime publishes namespace-scoped socket/interface snapshots |
| `pf-meminfo`, `pf-stat`, `pf-cpuinfo`, `pf-cpumodel`, `pf-rng`, `pf-misc`, `pf-sysctl`, `pf-sysfs` | System files derive from topology, ISA capability, entropy and system authorities | Minimal CPU/memory/uptime/UTS projections exist; fields and many leaves are incomplete | System authorities publish typed values; procfs/sysfs only format them |

## Implemented first invariant

`hl-vfs::Procfs::read_link` now resolves `/proc/self` from the `current`
identity supplied for that lookup and verifies that the process is live.
`Procfs::kind` reports the same node as a link. The focused `self_link_live`
test covers slash normalization, live identity, node kind, and stale identity
rejection. This is deliberately not a constant stub: a forked child receives a
different `current` value and therefore a different link target.

The same boundary now owns `/proc/<pid>/root`. Its target comes from the
application path namespace's typed `namespace_root` identity, is installed on
`TaskProcfs`, and is returned only after the target process is validated live.
The vfs unit test deliberately supplies `/sandbox`, proving the renderer does
not contain a fixture-specific `/` literal; production composition supplies its
guest namespace root `/`.

The next broad implementation should be a caller context containing both PID
and TID. It unlocks `/proc/thread-self` and the task directory without embedding
application-specific policy in procfs.
