#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

fn assert_consumed(scenario: u32, branch: &str) {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(scenario),
        0,
        "stale {branch} marker crossed a second dispatcher boundary"
    );
}

#[test]
fn conditional_jump_marker_is_consumed() { assert_consumed(247, "conditional jump"); }

#[test]
fn direct_jump_marker_is_consumed() { assert_consumed(248, "direct jump"); }

#[test]
fn return_marker_is_consumed() { assert_consumed(249, "return"); }

#[test]
fn indirect_branch_marker_is_consumed() { assert_consumed(250, "indirect branch"); }

#[test]
fn fallthrough_marker_is_consumed() { assert_consumed(251, "fallthrough"); }

#[test]
fn mapped_redispatch_consumes_the_miss_before_signal_handler_rip() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(252),
        0,
        "signal-handler RIP consumed the interrupted branch's stale marker"
    );
}

#[test]
fn mapped_redispatch_consumes_refused_miss_at_the_exact_target() {
    assert_eq!(hl_native::x86_64_translit_displaced_test(253), 0);
}

#[test]
fn teardown_finalizes_a_pending_fallthrough_miss() {
    assert_eq!(hl_native::x86_64_translit_displaced_test(254), 0);
}

#[test]
fn mapped_hit_dispatch_path_invokes_the_tested_commit_seam() {
    let source = include_str!("../src/native/engine/dispatch.c");
    let invocation = "dispatch_fast_redispatch_commit(c, next_code);";
    assert_eq!(
        source.matches(invocation).count(),
        1,
        "the mapped-hit dispatcher must have exactly one production commit invocation"
    );
    assert!(
        source.contains(concat!(
            "REDISPATCH_COUNT(REDISPATCH_HIT);\n",
            "                    dispatch_fast_redispatch_commit(c, next_code);"
        )),
        "the production commit must remain at the mapped-hit seam, before B executes"
    );
}
