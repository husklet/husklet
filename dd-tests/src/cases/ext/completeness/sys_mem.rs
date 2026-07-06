use super::*;

/// Memory-management syscalls.
pub(super) fn sys_mem() -> Group {
    group(
        "comp-sys-mem",
        vec![
            sy("mremap", "completeness/sys_mremap.c"),
            sy("mlock", "completeness/sys_mlock.c"),
            sy("madvise", "completeness/sys_madvise2.c"), // WILLNEED/SEQUENTIAL/FREE
            // GAP mincore: engine returns an error (ok=0); real Linux fills the residency vector (ok=1).
            sy("mincore", "completeness/sys_mincore.c"),
            // GAP membarrier: CMD_QUERY and CMD_GLOBAL both fail under the engine (query_ok=0 global=0).
            sy("membarrier", "completeness/sys_membarrier.c"),
            // process_vm_readv now works on both engines; the x86_64 qemu-user oracle lacks it (n=-1) so the
            // JIT's correct result (n=16) reads as a mismatch -> oracle artifact, not an engine gap (cf. clone3).
            sy("process-vm", "completeness/sys_process_vm.c").xfail(X86),
        ],
    )
}
