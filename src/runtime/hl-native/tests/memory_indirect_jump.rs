#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

use std::sync::{Mutex, OnceLock};

fn native_globals() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// This integration target is its own process. In particular, scenario 149 installs and exercises
/// signal-stage fixtures without sharing native globals with the library unit-test scheduler.
#[test]
fn classifier_and_signal_stages_are_exact() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(148), 0, "classifier fixture");
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(149),
        0,
        "signal-stage fixture"
    );
}

#[test]
fn sampling_exit_waits_for_an_inflight_record_commit() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(151), 0, "sampling publication barrier fixture");
}

#[test]
fn disabled_sampling_bypasses_helper_publication_state() {
    let _guard = native_globals();
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(185),
        0,
        "disabled profiling must stop before process identity and publication state"
    );
}

#[test]
fn sampling_options_are_snapshotted_once_per_execution() {
    let _guard = native_globals();
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(186),
        0,
        "hot publication paths must use the execution snapshot rather than resolving option names"
    );
}
