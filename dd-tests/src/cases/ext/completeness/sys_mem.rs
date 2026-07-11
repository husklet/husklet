use super::*;

/// Memory-management syscalls.
pub(super) fn sys_mem() -> Group {
    group(
        "comp-sys-mem",
        vec![
            sy("mremap", "completeness/sys_mremap.c"),
            sy("mlock", "completeness/sys_mlock.c"),
            sy("madvise", "completeness/sys_madvise2.c"), // WILLNEED/SEQUENTIAL/FREE
            sy("mincore", "completeness/sys_mincore.c"), // residency vector filled — oracle-identical to native
            sy("membarrier", "completeness/sys_membarrier.c"), // CMD_QUERY + CMD_GLOBAL — oracle-identical to native
            // process_vm_readv now works on both engines; the x86_64 qemu-user oracle lacks it (n=-1) so the
            // JIT's correct result (n=16) reads as a mismatch -> oracle artifact, not an engine gap (cf. clone3).
            sy("process-vm", "completeness/sys_process_vm.c").xfail(X86),
        ],
    )
}
