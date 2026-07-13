//! completeness — systematic syscall-table + opcode-space coverage. Owner: completeness agent.
//! Goal: prove which syscalls and which x86-64/aarch64 instructions the engine handles vs leaves
//! UNIMPL/unhandled. Uses COMPILED GUESTS (no docker images → zero disk cost). Edit ONLY this file.
//!
//! Two systematic suites:
//!  1. SYSCALL COMPLETENESS — each guest drives one syscall (or a tight family) via direct
//!     `syscall(SYS_x, …)` with safe deterministic args and prints a stable verdict, then `.oracle()`:
//!     the JIT run's stdout+exit must equal the same guest run natively (aarch64 direct, x86_64 via
//!     qemu). If the engine returns -ENOSYS / a wrong value / diverges, the verdict differs and the
//!     oracle catches it → that's an unhandled/buggy syscall, marked `.xfail()` + a docs/GAPS.md row.
//!  2. OPCODE COMPLETENESS — x86-64 guests (compiled by the x86_64 cross-gcc, run on LinuxX86_64, oracle
//!     vs qemu) and aarch64 guests (native gcc, run on LinuxAarch64, oracle vs native) compute a
//!     deterministic value over fixed inputs across the SIMD / crypto / bitmanip / atomics instruction
//!     space via `__attribute__((target(...)))` intrinsics + inline asm. A mistranslation (wrong value)
//!     OR an UNIMPL (crash/diag) diverges from the oracle and is caught.
//!
//! All guests live under guests/completeness/<name>.c and share guests/completeness/compat.h.
use dd_tests::{group, src, Case, Engine, Group};

// ---- SYSCALL COMPLETENESS ----
mod sys_cred;
mod sys_file;
mod sys_fs;
mod sys_mem;
mod sys_misc;
mod sys_proc;
mod sys_signal;
mod sys_time;
// ---- OPCODE COMPLETENESS ----
mod op_arm_ext;
mod op_arm_neon;
mod op_x86_avx;
mod op_x86_bit;
mod op_x86_crypto;
mod op_x86_misc;
mod op_x86_sse;

use op_arm_ext::op_arm_ext;
use op_arm_neon::op_arm_neon;
use op_x86_avx::op_x86_avx;
use op_x86_bit::op_x86_bit;
use op_x86_crypto::op_x86_crypto;
use op_x86_misc::op_x86_misc;
use op_x86_sse::op_x86_sse;
use sys_cred::sys_cred;
use sys_file::sys_file;
use sys_fs::sys_fs;
use sys_mem::sys_mem;
use sys_misc::sys_misc;
use sys_proc::sys_proc;
use sys_signal::sys_signal;
use sys_time::sys_time;

/// syscall guest: one source, run on BOTH Linux engines, oracle-diffed vs native.
pub(super) fn sy(name: &'static str, file: &'static str) -> Case {
    src(name, file).oracle()
}
/// x86-64 opcode guest: x86_64 engine only (x86 intrinsics don't compile for aarch64), oracle vs qemu.
pub(super) fn x(name: &'static str, file: &'static str) -> Case {
    src(name, file).only(&[Engine::LinuxX86_64]).oracle()
}
/// aarch64 opcode guest: aarch64 engine only, oracle vs native.
pub(super) fn a(name: &'static str, file: &'static str) -> Case {
    src(name, file).only(&[Engine::LinuxAarch64]).oracle()
}
pub(super) const X86: &[Engine] = &[Engine::LinuxX86_64];
pub(super) const ARM: &[Engine] = &[Engine::LinuxAarch64];

pub fn groups() -> Vec<Group> {
    vec![
        // ---- SYSCALL COMPLETENESS ----
        sys_file(),
        sys_proc(),
        sys_mem(),
        sys_signal(),
        sys_time(),
        sys_cred(),
        sys_fs(),
        sys_misc(),
        // ---- OPCODE COMPLETENESS ----
        op_x86_sse(),
        op_x86_avx(),
        op_x86_bit(),
        op_x86_crypto(),
        op_x86_misc(),
        op_arm_neon(),
        op_arm_ext(),
    ]
}
