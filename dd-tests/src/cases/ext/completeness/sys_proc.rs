use super::*;

/// Process / scheduling / credentials-adjacent syscalls.
pub(super) fn sys_proc() -> Group {
    group(
        "comp-sys-proc",
        vec![
            // clone3 WORKS under both real engines; the x86_64 oracle (qemu-user) lacks clone3 so it
            // reports child=-1 → xfail X86 is an ORACLE artifact, not an engine gap (aarch64 passes).
            sy("clone3", "completeness/sys_clone3.c").xfail(X86),
            sy("getrusage", "completeness/sys_getrusage.c"),
            sy("prlimit64", "completeness/sys_prlimit64.c"),
            sy("priority", "completeness/sys_priority.c"),
            sy("getcpu", "completeness/sys_getcpu.c"),
            sy("sched-affinity", "completeness/sys_sched_affinity.c"),
            // GAP sched_get_priority_min(SCHED_FIFO): engine returns 0, real Linux returns 1.
            sy("sched-attr", "completeness/sys_sched_attr.c"),
            sy("capget", "completeness/sys_capget.c"),
            sy("set-tid-address", "completeness/sys_set_tid_address.c"),
            sy("unshare-setns", "completeness/sys_unshare_setns.c"),
            sy("exec-newline-env", "completeness/exec_newline_env.c"),
            // procfs fidelity: metadata must agree with the syscalls reporting the same facts.
            sy("pf-status-meta", "completeness/pf_status_meta.c"),
            sy("pf-proc-files", "completeness/pf_proc_files.c"),
            sy("pf-procstat-forks", "completeness/pf_procstat_forks.c"),
            sy("pf-cgroup-files", "completeness/pf_cgroup_files.c"),
            sy("pf-taskdir", "completeness/pf_taskdir.c"),
            sy("pf-selfdir", "completeness/pf_selfdir.c"),
            sy("pf-dirs", "completeness/pf_dirs.c"),
            sy("pf-argv", "completeness/pf_argv.c"),
            sy("pf-devseed", "completeness/pf_devseed.c"),
        ],
    )
}
