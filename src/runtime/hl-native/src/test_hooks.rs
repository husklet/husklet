#![allow(unsafe_code)]

use super::c_int;
#[cfg(test)]
use super::c_uint;

#[cfg(feature = "native-test-hooks")]
pub(super) fn test_api() -> &'static crate::loader::TestApi {
    crate::loader::tests().unwrap_or_else(|error| panic!("native test bridge unavailable: {error}"))
}

/// Defines one call into a feature-gated native test hook.
///
/// These share a safety argument distinct from `engine_entry!`: `test_api()` resolves the hook
/// table eagerly and panics rather than returning when the loaded object does not export it, so
/// a generated body never has an absent case and always calls a live export of the declared
/// signature. Each hook owns its own fixture -- its descriptors, mappings, threads and child
/// processes -- for the whole call and has released or reaped all of them before it returns a
/// scalar status, so nothing it touches outlives the call and no Rust storage is aliased. The
/// generated function is `unsafe`, so its caller owns the pointer arguments it forwards; the C
/// side reports failure as a status and never unwinds across the boundary.
macro_rules! test_entry {
    ($(#[$attribute:meta])* $name:ident($($argument:ident: $type:ty),* $(,)?) -> $result:ty, $field:ident) => {
        $(#[$attribute])*
        pub(crate) unsafe fn $name($($argument: $type),*) -> $result {
            // SAFETY: a resolved hook is a live export of this signature owning its own fixture;
            // the caller of this `unsafe fn` owns the arguments. Stated in full on `test_entry!`.
            unsafe { (test_api().$field)($($argument),*) }
        }
    };
    ($(#[$attribute:meta])* $name:ident($($argument:ident: $type:ty),* $(,)?), $field:ident) => {
        $(#[$attribute])*
        pub(crate) unsafe fn $name($($argument: $type),*) {
            // SAFETY: a resolved hook is a live export of this signature owning its own fixture;
            // the caller of this `unsafe fn` owns the arguments. Stated in full on `test_entry!`.
            unsafe { (test_api().$field)($($argument),*) };
        }
    };
}

test_entry!(#[cfg(feature = "native-test-hooks")]
#[allow(dead_code)]
hl_c_backend_checkpoint_peer_authenticate_test(descriptor: c_int, claimed_pid: u64, host_pid: *mut u64, host_birth: *mut u64) -> c_int, checkpoint_peer_authenticate);

test_entry!(#[cfg(feature = "native-test-hooks")]
hl_c_backend_checkpoint_channel_connect_test(broker_child: c_int) -> c_int, checkpoint_channel_connect);

test_entry!(#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
hl_c_backend_checkpoint_process_identity_open_test(pid: c_int, expected_birth: u64, expected_generation: u64, actual_birth: *mut u64, actual_generation: *mut u64) -> c_int, checkpoint_process_identity_open);

test_entry!(#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
hl_c_backend_checkpoint_peer_identity_open_test(descriptor: c_int, claimed_pid: u64, actual_pid: *mut u64, actual_birth: *mut u64, actual_generation: *mut u64) -> c_int, checkpoint_peer_identity_open);

#[cfg(all(feature = "native-test-hooks", target_os = "macos"))]
pub(crate) fn directory_stream_private_test(scenario: u32) -> i32 {
    // SAFETY: this feature-gated hook owns its host context and directory fixture end to end.
    unsafe { (test_api().directory_stream_private)(scenario) }
}

test_entry!(#[cfg(all(test, feature = "native-test-hooks"))]
hl_c_backend_checkpoint_test_prune_foreign_descriptors() -> c_uint, checkpoint_test_prune_foreign_descriptors);

test_entry!(
    #[cfg(all(test, feature = "native-test-hooks"))]
    hl_c_backend_checkpoint_test_fail_registry_allocation(),
    checkpoint_test_fail_registry_allocation
);

test_entry!(#[cfg(all(test, feature = "native-test-hooks"))]
hl_c_backend_checkpoint_test_fail_private_adopt(position: c_uint), checkpoint_test_fail_private_adopt);

test_entry!(#[cfg(all(test, feature = "native-test-hooks"))]
hl_c_backend_checkpoint_test_private_descriptor_count() -> u64, checkpoint_test_private_descriptor_count);

test_entry!(#[cfg(all(test, feature = "native-test-hooks", unix))]
hl_c_backend_host_process_force_test(pid: c_int) -> c_int, host_process_force);

// Only `a_peer_that_leads_its_own_session_is_still_enumerated_as_a_peer` calls this, and that
// test is Linux-only because it reads the coordinator's /proc view; the C hook exists on both
// hosts, so the cfg tracks the caller rather than the symbol.
test_entry!(#[cfg(all(test, feature = "native-test-hooks", target_os = "linux"))]
hl_c_backend_host_process_peer_enumerated_test(pid: c_int) -> c_int, host_process_peer_enumerated);

test_entry!(#[cfg(all(test, feature = "native-test-hooks"))]
hl_c_backend_activation_ready_pause(paused: c_int), activation_ready_pause);

/// Runs the ISA-selected scenario hook and maps its scalar status onto a result.
///
/// One safety statement covers this helper and its no-argument sibling, which is why the sixteen
/// callers below carry none of their own. Both hooks are resolved entries of `TestApi`, so each is
/// a live export of the declared signature in an object that stays mapped for the process. A hook
/// owns its whole fixture -- descriptors, mappings, threads and child processes -- for the call and
/// has closed, unmapped, joined or reaped every one before returning, so it aliases no Rust storage
/// and leaves nothing behind. `scenario` is a scalar selector, the return is a scalar status, and
/// the C side never unwinds across the boundary.
#[cfg(feature = "native-test-hooks")]
pub(super) fn scenario_status(
    isa: u32,
    aarch64: crate::loader::ScenarioTest,
    x86_64: crate::loader::ScenarioTest,
    scenario: u32,
) -> Result<(), i32> {
    let hook = match isa {
        1 => aarch64,
        2 => x86_64,
        _ => return Err(-22),
    };
    // SAFETY: stated in full on this function's documentation.
    let status = unsafe { hook(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

/// Runs the ISA-selected hook that takes no scenario, under `scenario_status`'s safety statement.
#[cfg(feature = "native-test-hooks")]
pub(super) fn no_argument_status(
    isa: u32,
    aarch64: crate::loader::NoArgumentTest,
    x86_64: crate::loader::NoArgumentTest,
) -> Result<(), i32> {
    let hook = match isa {
        1 => aarch64,
        2 => x86_64,
        _ => return Err(-22),
    };
    // SAFETY: stated in full on `scenario_status`.
    let status = unsafe { hook() };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn bound_vector_io_test(isa: u32, scenario: u32) -> Result<(i64, u32, u64), i32> {
    let (mut result, mut calls, mut bytes) = (i64::MIN, u32::MAX, u64::MAX);
    let hook = match isa {
        1 => test_api().aarch64_bound_vector_io,
        2 => test_api().x86_64_bound_vector_io,
        _ => return Err(-22),
    };
    // SAFETY: the feature-gated C hook accepts writable scalar outputs and owns its fixture memory.
    let status = unsafe { hook(scenario, &raw mut result, &raw mut calls, &raw mut bytes) };
    if status == 0 {
        Ok((result, calls, bytes))
    } else {
        Err(status)
    }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn identity_registry_test(scenario: u32, iterations: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook owns its private shared registry and child processes. Inputs are
    // scalar scenario controls, and the hook returns only after every child has been reaped.
    let status = unsafe { (test_api().identity_registry)(scenario, iterations) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn setfl_append_write_test(scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook owns the descriptor table, the host services and the kernel
    // objects it builds, and releases all of them before returning. Its input is a scalar scenario selector.
    let status = unsafe { (test_api().setfl_append_write)(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn process_identity_token_test(scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook reads only /proc records for this process and pid 1, and owns
    // the one child it forks, reaping it before returning. Its input is a scalar scenario selector.
    let status = unsafe { (test_api().process_identity_token)(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn private_fork_lock_test(scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook owns its own descriptor, thread, and child process, and
    // returns only after the child has been reaped and the holder thread joined.
    let status = unsafe { (test_api().private_fork_lock)(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_channel_notify_test(isa: u32, scenario: u32) -> Result<(), i32> {
    // SAFETY: the feature-gated native hook owns both ends of its socket fixture and returns a scalar.
    scenario_status(
        isa,
        test_api().aarch64_checkpoint_channel_notify,
        test_api().x86_64_checkpoint_channel_notify,
        scenario,
    )
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn namespace_transaction_test(isa: u32, scenario: u32) -> Result<(), i32> {
    let hook = match isa {
        1 => test_api().aarch64_namespace_transaction,
        2 => test_api().x86_64_namespace_transaction,
        _ => return Err(-22),
    };
    // SAFETY: each feature-gated hook owns its shared transaction fixture and
    // reaps every child before returning a scalar status.
    let status = unsafe { hook(scenario) };
    if status == 0 { Ok(()) } else { Err(status) }
}

#[cfg(all(test, feature = "native-test-hooks"))]
pub(crate) fn fdvis_path_publication_test(isa: u32, scenario: u32) -> bool {
    let hook = match isa {
        1 => test_api().aarch64_fdvis_path_publication,
        2 => test_api().x86_64_fdvis_path_publication,
        _ => return false,
    };
    // SAFETY: the feature-gated hook owns and restores its isolated descriptor-path fixture.
    unsafe { hook(scenario) == 1 }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn proc_fdinfo_listing_test(isa: u32, scenario: u32) -> i32 {
    let hook = match isa {
        1 => test_api().aarch64_proc_fdinfo_listing,
        2 => test_api().x86_64_proc_fdinfo_listing,
        _ => return -(libc::EINVAL),
    };
    // SAFETY: the hook owns its descriptors and restores its eventfd binding and ledger rows.
    unsafe { hook(scenario) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn exec_page_cache_test(isa: u32, scenario: u32) -> Result<u64, i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let hook = match isa {
        1 => test_api().aarch64_exec_page_cache,
        2 => test_api().x86_64_exec_page_cache,
        _ => return Err(-22),
    };
    let mut scans = 0;
    // SAFETY: the hook serially owns and restores the non-executable-range fixture and writes one scalar.
    let status = unsafe { hook(scenario, &raw mut scans) };
    if status == 0 { Ok(scans) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn x86_store_preflight_test() -> i32 {
    // SAFETY: the feature-gated hook owns its local emitter and CPU fixtures.
    unsafe { (test_api().x86_64_store_preflight)() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn aarch64_reserved_register_test() -> i32 {
    // SAFETY: the feature-gated hook owns its local emitter buffer and restores every global it moves.
    unsafe { (test_api().aarch64_reserved_register)() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn x86_reserved_register_test() -> i32 {
    // SAFETY: the feature-gated hook owns its local emitter buffer and restores every global it moves.
    unsafe { (test_api().x86_64_reserved_register)() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn aarch64_imported_path_guard_test() -> i32 {
    // SAFETY: the hook owns its own cpu fixture and heap pathname, and clears the ledger interval it arms.
    unsafe { (test_api().aarch64_imported_path_guard)() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn x86_imported_path_guard_test() -> i32 {
    // SAFETY: the hook owns its own cpu fixture and heap pathname, and clears the ledger interval it arms.
    unsafe { (test_api().x86_64_imported_path_guard)() }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn checkpoint_logical_snapshot_test(isa: u32, scenario: u32) -> Result<u64, i32> {
    let hook = match isa {
        1 => test_api().aarch64_checkpoint_logical_snapshot,
        2 => test_api().x86_64_checkpoint_logical_snapshot,
        _ => return Err(-1),
    };
    let mut visits = 0;
    // SAFETY: the hook owns its synthetic descriptor array and writes one scalar result.
    let status = unsafe { hook(scenario, &raw mut visits) };
    if status == 0 { Ok(visits) } else { Err(status) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn linux_errno_from_host(domain: u32, host_errno: i32) -> i32 {
    // SAFETY: this pure test export accepts and returns one scalar value.
    unsafe { (test_api().errno_from_host)(domain, host_errno) }
}

#[cfg(feature = "native-test-hooks")]
pub(crate) fn signal_errno_frame_test(
    isa: u32,
    domain: u32,
    redirect: bool,
    nr: u64,
    raw: i64,
) -> Result<(i64, i64), i32> {
    let hook = match isa {
        1 => test_api().aarch64_signal_errno_frame,
        2 => test_api().x86_64_signal_errno_frame,
        _ => return Err(-22),
    };
    let (mut observed, mut completed) = (i64::MIN, i64::MIN);
    // SAFETY: the feature-gated hook owns its CPU fixture and writes two scalar outputs.
    let status = unsafe {
        hook(
            domain,
            u32::from(redirect),
            nr,
            raw,
            &raw mut observed,
            &raw mut completed,
        )
    };
    if status == 0 {
        Ok((observed, completed))
    } else {
        Err(status)
    }
}
