# Isolation oracle audit

## Retained implementation studied

The retained engine was inspected read-only. The isolation policy is C-only;
the retained tree contains no `.S` or `.s` files. Both guest ISAs enter the
same C syscall and virtual-filesystem implementation.

- `../engine/src/linux_abi/container/state.c`: the process-global
  `g_hostname`, `g_mem_max`, `g_cpu_max`, `g_pids_max`, `g_limits`,
  `g_init_hostpid`, `container_pid`, `container_online_cpus`,
  `container_read_resource_env`, `parse_ulimits`, `rootfs_ro_denies`,
  `acct_container_reset`, `acct_child_born`, and `acct_after_fork` state and
  transitions.
- `../engine/src/linux_abi/container/vfs.c`: `proc_open`,
  `proc_root_dir_open`, `proc_status_text`, `proc_stat_text`,
  `proc_limits_text`, `cgroup_procs_text`, `proc_reg_publish`,
  `proc_reg_after_fork`, `proc_reg_mark_child`, and `proc_reg_reap`.
- `../engine/src/linux_abi/limits.c` and `limits.h`:
  `hl_limit_table_init`, `hl_limit_table_set`, and `hl_limit_table_get`, whose
  sequence counter publishes an atomic soft/hard pair.
- `../engine/src/linux_abi/syscall/misc.c`: `hl_linux_misc_dispatch` cases 160
  and 161 for `uname` and `sethostname`; case 160 returns `-EFAULT` on a short
  guest copy and case 161 rejects an unavailable hostname store with
  `-EINVAL`.
- `../engine/src/linux_abi/syscall/dispatch.c`: `linux_online_cpus`, the
  process-global `g_affinity`, and the affinity/getcpu dispatch paths. The CPU
  limit is shared with procfs/sysfs through `container_online_cpus`.
- `../engine/src/linux_abi/syscall/rare.c` and
  `../engine/src/linux_abi/syscall/proc.c`: direct get/setrlimit handling and
  `prlimit64` case 261. `prlimit64` validates the target, resource, and guest
  buffers before mutation, copies the old pair before applying the new pair,
  rejects soft greater than hard with `EINVAL`, and rejects a nofile increase
  beyond the engine ceiling with `EPERM`.
- `../engine/src/linux_abi/fork.c`: child-side repair calls
  `container_pid_after_fork`, `proc_reg_after_fork`, and `acct_after_fork` after
  host fork. `../engine/src/linux_abi/linux_abi.c::hl_linux_abi_spawn` prepares
  the open-file-description fork plan, enters `hl_linux_spawn_entry`, then
  completes the parent plan. `../engine/src/core/lifecycle.c` functions
  `hl_production_start_process`, `hl_production_finish_process`, and
  `hl_production_result_release` own host-process launch, wait-result capture,
  and shared-result mapping teardown.

## Ownership and ordering

The retained design keeps namespace/resource configuration in process-global
state. Forked host processes inherit it. A shared atomic accounting arena gives
each host process one PID-keyed slot for task and memory totals; the parent
pre-registers a child before returning from fork, and the child claims that
slot after fork. Proc membership is separately published as an atomically
renamed file keyed by host PID. Exec republishes command identity; normal exit
unlinks its records through `atexit`, while parent reap removes records for a
signal-killed child. PID reuse is therefore guarded at reap, not merely at
lookup.

The retained proc/sysfs/cgroup renderer reads these globals and registry files
when a virtual file is opened, writes a bounded body into a temporary regular
file, and lets the open file description retain the read offset. The limit
table is sequence-protected, but the broader retained configuration is not an
instance-owned locked object. Rust must preserve the guest behavior without
copying that global-state ownership flaw.

Malformed launch values fail at the configuration boundary. Guest operations
preserve `ENOENT` for absent virtual paths, `EFAULT` for inaccessible buffers,
`EINVAL` for invalid resources or limit pairs, `EPERM` for forbidden limit
raises, and Linux partial-read/open-file-offset behavior. AArch64 and x86-64
share isolation policy; their intended differences are the reported machine
and CPU model. POSIX uses real host fork/PID liveness behind the guest identity;
the Windows production path instead serializes a cold launch and does not use
`hl_linux_abi_spawn`.

## Rust mapping and remaining evidence gap

| Retained capability | Rust owner | State |
|---|---|---|
| PID, credential, session, UTS, fork/exec and teardown lifecycle | `hl-task::TaskRegistry`; `hl-runtime` process adapters | implemented as instance-owned, generation-qualified state; isolation rows not yet executed to completion |
| Proc/sysfs rendering and confined address/mount views | `hl-vfs::Procfs` and its consumer-owned `ProcfsSource`; `hl-runtime::TaskProcfs` | implemented with bounded value snapshots; typed-native isolation evidence blocked |
| CPU topology and affinity agreement | `hl-task::CpuTopology`/`CpuAffinity`; `hl-runtime::TaskProcfs::cpu` | implemented; both-ISA row evidence blocked |
| Unified cgroup membership and configured CPU/memory projection | `hl-vfs` cgroup `View`; `hl-runtime::TaskProcfs::cgroup`; `SystemAuthority` | implemented projection; both-ISA row evidence blocked |
| Rlimit syscall/proc agreement | `hl-task::ProcessLimits`; `hl-runtime` process routing; `TaskProcfs::process` | implemented ownership and projection; configured row evidence blocked |
| Rootfs read-only and masked-path image behavior | container/image filesystem composition | retained only as four explicit external-service rows in `images.tsv`; not run by the C-case runner |

The folder preserves all 21 retained C case identities for both ISAs (42
rows), plus four image-rootfs contracts. These are acceptance contracts, not
proof that the whole isolation domain is complete. Promotion to active requires
a complete typed-native run with durable row results from an exact committed
tree.
