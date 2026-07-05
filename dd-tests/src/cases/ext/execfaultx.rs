//! execfaultx — fork->execve child fault delivery + CRASHDBG Mach exception-port delivery.
//! Owner: fork/exec-fault agent. Edit ONLY this file. Keep it compiling (`cargo build -p dd-tests`).
//!
//! A compiler DRIVER (gcc/clang) forks and execve's sub-processes (cc1/as/ld/collect2); those exec'd
//! CHILDREN take guest CPU faults -- some HANDLED via a registered SIGSEGV handler (cc1's fatal-signal
//! handler, glibc stack-overflow detection), some unhandled. dd must re-establish the faulting thread's
//! signal + Mach-exception state across fork + in-process execve and deliver both fates exactly as Linux.
//!
//! - `execfault` (no CRASHDBG): the driver forks, the child execve's a fresh image and faults. A HANDLED
//!   fault must reach the guest handler (child _exit(42)); an UNHANDLED one must kill the child with a
//!   guest SIGSEGV so the parent's wait4 reconstructs WIFSIGNALED/WTERMSIG=11. Byte-exact vs the native
//!   oracle on both Linux engines (golden below == `handled: exited 42\nunhandled: signal 11`).
//!
//! - `execfault-crashdbg` (env CRASHDBG=1, `mainhandled`): a guest-handled fault on the MAIN thread under
//!   CRASHDBG. On aarch64 CRASHDBG the fault is caught by a Mach exception port whose handler runs on a
//!   DEDICATED exc_thread -- so deliver_guest_fault had to be told the FAULTING thread's cpu (recovered
//!   from its x28==CPUREG) instead of reading the exc_thread's (NULL) TLS. Pre-fix this path declined the
//!   guest handler and aborted with a spurious `[MACH]` (empty stdout, exit 139); post-fix it delivers.
//!   On x86-64 CRASHDBG the same fault is served by the POSIX diag_crash handler (runs on the faulting
//!   thread) -- a companion check that the guest handler is delivered on both engines.

use crate::{group, src, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![group(
        "execfaultx",
        vec![
            src("execfault", "execfault.c")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .out("handled: exited 42\nunhandled: signal 11\n"),
            src("execfault-crashdbg", "execfault.c")
                .args(&["mainhandled"])
                .env("CRASHDBG", "1")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .out("mainhandled ok\n"),
        ],
    )]
}
