use super::*;

/// Credential / identity syscalls (verdicts are structural so they're host-uid-invariant).
pub(super) fn sys_cred() -> Group {
    group(
        "comp-sys-cred",
        vec![
            sy("getresuid", "completeness/sys_getresuid.c"),
            sy("getresuid-null", "completeness/sys_getresuid_null.c"),
            sy("setfsuid", "completeness/sys_setfsuid.c"),
            sy("getgroups", "completeness/sys_getgroups.c"),
        ],
    )
}
