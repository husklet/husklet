#![cfg(all(feature = "native-test-hooks", unix))]

use std::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn fatal_signal_census_tail_is_atomic_only() {
    let source = include_str!("../src/native/linux_abi/signal.c");
    let body = source
        .split_once("static _Noreturn void guest_group_fatal")
        .and_then(|(_, tail)| tail.split_once("\n}\n\n// SA_SIGINFO"))
        .map(|(body, _)| body)
        .expect("guest_group_fatal body");
    let tail = body
        .split_once("ckpt_restored_member_exit_signal(sig);")
        .map(|(_, tail)| tail)
        .expect("existing restored-member signal publication");
    assert!(tail.contains("hl_backend_tree_finalize(1)"), "{tail}");
    assert!(tail.contains("_exit(128 + sig)"), "{tail}");
    for forbidden in [
        "launch_reg_terminate_peers",
        "hl_backend_tree_report",
        "waitpid",
        "kill(",
        "poll(",
        "snprintf",
        "opendir",
        "readdir",
        "open(",
        "read(",
        "unlink",
    ] {
        assert!(!tail.contains(forbidden), "fatal census tail calls {forbidden}: {tail}");
    }
    assert!(
        tail.find("hl_backend_tree_finalize(1)") < tail.find("_exit(128 + sig)"),
        "{tail}"
    );
}

#[test]
fn ordinary_and_nested_processes_share_execution_counters() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        for scenario in [0, 1] {
            hl_native::backend_tree_census_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} backend-tree scenario {scenario} failed: {status}"));
        }
    }
}

#[test]
fn unfinalized_and_explicitly_abnormal_processes_have_distinct_lifecycle_rows() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        for scenario in [2, 3, 5, 6, 7] {
            hl_native::backend_tree_census_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} backend-tree scenario {scenario} failed: {status}"));
        }
    }
}

#[test]
fn duplicate_finalize_is_counted_without_changing_the_first_outcome() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 4)
            .unwrap_or_else(|status| panic!("ISA {isa} backend-tree duplicate-finalize scenario failed: {status}"));
    }
}

#[test]
fn backend_shape_aggregates_nested_processes_and_keyed_forms() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 8)
            .unwrap_or_else(|status| panic!("ISA {isa} backend-shape aggregation scenario failed: {status}"));
    }
}

#[test]
fn publication_would_link_dispositions_reconcile_across_nested_processes() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 9)
            .unwrap_or_else(|status| panic!("ISA {isa} would-link aggregation scenario failed: {status}"));
    }
}

#[test]
fn executed_family_counts_aggregate_across_forks_and_ignore_top8_saturation() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 10)
            .unwrap_or_else(|status| panic!("ISA {isa} executed-family aggregation scenario failed: {status}"));
    }
}

#[test]
fn executed_fall_stop_reasons_reconcile_exactly_to_translated_fallthroughs() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 11)
            .unwrap_or_else(|status| panic!("ISA {isa} fall-stop reconciliation scenario failed: {status}"));
    }
}

#[test]
fn each_real_translation_stop_site_keeps_its_exact_reason() {
    // The hook scenarios exercise marker transport and exact counter reconciliation. This wiring
    // clamp makes a mutation at any real build-loop assignment red as well: replacing one site by
    // OTHER must not be masked by the sum invariant.
    let source = include_str!("../src/native/translator/guest/x86_64/translit.inc");
    for assignment in [
        "fall_stop = HL_BACKEND_FALL_DECODE;",
        "fall_stop = kind == TL_SSE2 ? HL_BACKEND_FALL_FS_TO_SSE2 : HL_BACKEND_FALL_FS_TO_NORMAL;",
        "fall_stop = previous_kind == TL_SSE2 ? HL_BACKEND_FALL_SSE2_TO_FS",
        "fall_stop = current_sse2 ? HL_BACKEND_FALL_NORMAL_TO_SSE2",
        "fall_stop = HL_BACKEND_FALL_DISPLACED_UNSAFE;",
        "fall_stop = HL_BACKEND_FALL_FETCH;",
        "fall_stop = HL_BACKEND_FALL_RIPREL_LOWER;",
        "fall_stop = HL_BACKEND_FALL_FS_TRANSACTION;",
        "fall_stop = HL_BACKEND_FALL_SSE_RIPREL_LOWER;",
        "fall_stop = HL_BACKEND_FALL_CAP;",
    ] {
        assert_eq!(
            source.matches(assignment).count(),
            1,
            "missing or duplicated real stop site: {assignment}"
        );
    }
    assert_eq!(source.matches("fall_stop = HL_BACKEND_FALL_TL_NO;").count(), 1);
}

#[test]
fn executed_family_hooks_follow_interpreter_and_dispatcher_commit_boundaries() {
    let interpreter = include_str!("../src/native/translator/guest/x86_64/interp.c");
    let execute = interpreter
        .split_once("static void interp_execute(hl_x86_hot_context *context, struct cpu *cpu) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\n// run_block"))
        .map(|(body, _)| body)
        .expect("interp_execute body");
    assert!(
        execute.find("int step = interp_step(cpu, &insn").unwrap()
            < execute
                .find("interp_backend_family_completed(cpu, &insn, step)")
                .unwrap(),
        "family attribution must occur only after a faulting step returns"
    );

    let dispatcher = include_str!("../src/native/translator/guest/x86_64/interp_dispatch.h");
    for (reason, next_reason, kind) in [
        ("if ((c)->reason == R_DIV)", "if ((c)->reason == R_IDIV)", "UNSIGNED"),
        ("if ((c)->reason == R_IDIV)", "if ((c)->reason == R_TRAP)", "SIGNED"),
    ] {
        let arm = dispatcher
            .split_once(reason)
            .and_then(|(_, tail)| tail.split_once(next_reason))
            .map(|(body, _)| body)
            .expect("divide dispatcher arm");
        let rax = arm.find("(c)->r[RAX] =").unwrap();
        let rdx = arm.find("(c)->r[RDX] =").unwrap();
        let completed = arm
            .find(&format!(
                "hl_backend_tree_family_div_service64_completed(HL_BACKEND_FAMILY_DIV_{kind})"
            ))
            .unwrap();
        assert!(
            rax < completed && rdx < completed,
            "{reason} completion precedes register commit"
        );
        assert!(
            completed < arm.rfind("continue").unwrap(),
            "{reason} completion follows dispatch"
        );
    }
}

#[test]
fn product_executed_form_writers_remain_inside_the_existing_diagnostics_gate() {
    let source = include_str!("../src/native/translator/guest/x86_64/interp.c");
    let execute = source
        .split_once("static void interp_execute(hl_x86_hot_context *context, struct cpu *cpu) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\n// run_block"))
        .map(|(body, _)| body)
        .expect("interp_execute body");
    let committed = execute
        .split_once("if (census_steps && step != STEP_END) {")
        .and_then(|(_, tail)| tail.split_once("\n        }"))
        .map(|(body, _)| body)
        .expect("committed STEP_NEXT diagnostics branch");
    assert!(committed.contains("hl_backend_tree_executed_form(translit_unsupported_key(&insn))"));
    let terminal = execute
        .split_once("if (step == STEP_END) {")
        .map(|(_, body)| body)
        .expect("STEP_END branch");
    assert!(terminal.contains("if (census_steps && cpu->irq == 0)"));

    let backend = include_str!("../src/native/engine/backend_tree.c");
    let writer = backend
        .split_once("static inline void hl_backend_tree_executed_form(uint64_t key) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_backend_executed_form_top"))
        .map(|(body, _)| body)
        .expect("product executed-form writer");
    assert!(writer.contains("if (census == NULL || g_backend_mixed_sse_self == NULL) return;"));
}
