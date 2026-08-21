#![cfg(feature = "native-test-hooks")]

//! `svc_fs` imports every pathname operand out of guest memory into engine storage before dispatch,
//! and reaches the guest's own `EFAULT`/`ENAMETOOLONG` there. A handler that re-probes the imported
//! operand asks the guest `PROT_NONE` ledger about engine memory -- and that ledger does cover engine
//! memory, because `munmap` re-adds a released guest range to it and the host allocator later places
//! the engine's own thread stacks in that same free address space. Under `npm ci` that turned
//! `chmod` on a freshly unpacked package binary into `EFAULT` after thousands of successful syscalls
//! on the same paths, on whichever libuv thread-pool thread drew a poisoned stack.
//!
//! Unlike the emitter fixtures, these hooks answer on every host: they exercise the syscall layer,
//! which both target translation units compile everywhere.

/// The filesystem verdict for an absent pathname. `1` is the spurious `EFAULT` this gate exists for;
/// `2` and `5` are unexpected/vacuous and must fail just as loudly.
const FILESYSTEM_VERDICT: i32 = 0;

#[test]
fn an_imported_x86_64_pathname_is_not_judged_against_the_guest_protection_ledger() {
    assert_eq!(hl_native::x86_imported_path_guard_test(), FILESYSTEM_VERDICT);
}

#[test]
fn an_imported_aarch64_pathname_is_not_judged_against_the_guest_protection_ledger() {
    assert_eq!(hl_native::aarch64_imported_path_guard_test(), FILESYSTEM_VERDICT);
}
