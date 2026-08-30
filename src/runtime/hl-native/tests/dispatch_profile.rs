#![cfg(feature = "native-test-hooks")]

//! The dispatcher diagnostics are observational only. These target-specific hooks prove that
//! sampling, reason accounting, monotonic clock deltas, and the disabled fast path share the exact
//! accumulator compiled into each engine library.

#[test]
fn x86_64_dispatch_profile_accumulates_exact_raw_values() {
    assert_eq!(hl_native::dispatch_profile_test(2), 0);
}

#[test]
fn aarch64_dispatch_profile_accumulates_exact_raw_values() {
    assert_eq!(hl_native::dispatch_profile_test(1), 0);
}

#[test]
fn fall_redispatch_guards_are_ordered_and_bounded() {
    let source = include_str!("../src/native/engine/dispatch.c");
    let body = source
        .split_once("if (c->reason == R_BRANCH) {")
        .and_then(|(_, tail)| tail.split_once("// async signal -> guest handler"))
        .map(|(body, _)| body)
        .expect("fall redispatch decision");
    let required = [
        ("if (c->exited)", "REDISPATCH_COUNT(REDISPATCH_EXITED)"),
        ("else if (g_threaded)", "REDISPATCH_COUNT(REDISPATCH_THREADED)"),
        ("else if (c->irq != 0)", "REDISPATCH_COUNT(REDISPATCH_IRQ)"),
        ("else if (redispatch_chain >= 8)", "REDISPATCH_COUNT(REDISPATCH_BUDGET)"),
        ("hl_fatal_status(&g_jit_fatal) != HL_STATUS_OK", "REDISPATCH_COUNT(REDISPATCH_FATAL)"),
        ("signal_deliverable_for_cpu(c)", "REDISPATCH_COUNT(REDISPATCH_SIGNAL)"),
        ("next_code == NULL", "REDISPATCH_COUNT(REDISPATCH_MAP_MISS)"),
        ("next_generation != g_cache_gen", "REDISPATCH_COUNT(REDISPATCH_STALE)"),
    ];
    let mut previous = 0;
    for (guard, counter) in required {
        let guard_at = body.find(guard).unwrap_or_else(|| panic!("missing redispatch guard {guard}"));
        let counter_at = body.find(counter).unwrap_or_else(|| panic!("missing decline counter {counter}"));
        assert!(guard_at >= previous, "redispatch guard order changed at {guard}");
        assert!(counter_at > guard_at, "{guard} is not classified by {counter}");
        previous = guard_at;
    }
    assert!(body.contains("REDISPATCH_COUNT(REDISPATCH_ATTEMPTED)"));
    assert!(body.contains("REDISPATCH_COUNT(REDISPATCH_HIT)"));
    assert!(body.contains("redispatch_chain++"));
    assert!(body.contains("goto redispatch_execute"));
}
