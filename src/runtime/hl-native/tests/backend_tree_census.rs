#![cfg(all(feature = "native-test-hooks", unix))]

use std::sync::Mutex;

#[test]
fn aarch64_successful_terminal_outcomes_are_explicitly_retired() {
    let source = std::fs::read_to_string("src/native/translator/guest/aarch64/interp/integer/control.c").unwrap();
    let svc = source.split("cpu->reason = R_SYSCALL;").nth(1).expect("SVC outcome");
    assert!(svc.trim_start().starts_with("return INTERP_RETIRED_END;"));
    for reason in ["cpu->reason = R_ICCOMMIT;", "cpu->reason = R_ICFLUSH;"] {
        let tail = source.split(reason).nth(1).unwrap_or_else(|| panic!("missing {reason}"));
        assert!(tail.trim_start().starts_with("return INTERP_RETIRED_END;"), "{reason}");
    }
}

#[test]
fn aarch64_diagnostics_off_loop_has_no_census_operation() {
    let source = std::fs::read_to_string("src/native/translator/guest/aarch64/interp/dispatch.c").unwrap();
    let off = source.split("if (!hl_backend_tree_steps_enabled())").nth(1).expect("off loop");
    let off = off.split("} else {").next().expect("diagnostic loop boundary");
    assert!(off.contains("interp_step(cpu)"));
    assert!(!off.contains("major") && !off.contains("census") && !off.contains("body_retired"));
}

static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn x86_interpreter_step_forms_have_one_diagnostics_only_writer() {
    let source = include_str!("../src/native/translator/guest/x86_64/interp.c");
    let writer = "hl_backend_tree_executed_step_form(translit_step_form_key_v2(&insn));";
    assert_eq!(source.matches(writer).count(), 1, "step-form writer must have one authoritative site");
    let guarded = source
        .split_once("if (census_steps && step != STEP_END) {")
        .and_then(|(_, tail)| tail.split_once("\n        }"))
        .map(|(body, _)| body)
        .expect("interpreted-step census guard");
    assert!(guarded.contains("g_dispatch_census_interp_steps++;"), "{guarded}");
    assert!(guarded.contains("hl_backend_tree_executed_form(translit_unsupported_key(&insn));"), "{guarded}");
    assert!(guarded.contains(writer), "{guarded}");
    assert!(guarded.contains("#if !defined(HL_NATIVE_TEST_HOOKS)"), "{guarded}");
}

#[test]
fn x86_step_form_record_is_versioned_bounded_and_reconciled() {
    let source = include_str!("../src/native/engine/backend_tree.c");
    for contract in [
        "#define HL_BACKEND_EXECUTED_STEP_FORM_TOP 64u",
        "[diag] x86-executed-step-form version=2",
        "keyed + overflow == total",
        "total == interpreted",
        "top_cumulative <= keyed",
        "executed_step_form_total",
        "executed_step_form_overflow",
        "executed_step_forms[HL_BACKEND_EXECUTED_FORM_SLOTS]",
        "if (atomic_load_explicit(&census->executed_step_form_total, memory_order_relaxed) != 0)",
    ] {
        assert!(source.contains(contract), "missing step-form census contract: {contract}");
    }
    assert_eq!(source.matches("[diag] x86-executed-step-form version=2").count(), 1);
}

#[test]
fn x86_step_form_v2_adds_only_the_missing_sib_facts() {
    let source = include_str!("../src/native/translator/guest/x86_64/translit.inc");
    let encoder = source
        .split_once("static uint64_t translit_step_form_key_v2(const struct insn *insn) {")
        .and_then(|(_, tail)| tail.split_once("\n}"))
        .map(|(body, _)| body)
        .expect("v2 step-form encoder");
    assert!(encoder.contains("translit_unsupported_key(insn)"), "{encoder}");
    assert_eq!(encoder.matches("insn->m_hasbase != 0").count(), 1, "{encoder}");
    assert_eq!(encoder.matches("insn->m_hasindex != 0").count(), 1, "{encoder}");
    assert!(encoder.contains("<< 60"), "{encoder}");
    assert!(encoder.contains("<< 61"), "{encoder}");
    assert!(!encoder.contains("<< 62") && !encoder.contains("<< 63"), "{encoder}");

    let v1 = 0x0000_0002_0919_008b_u64;
    let absent = v1;
    let base = v1 | (1_u64 << 60);
    let index = v1 | (1_u64 << 61);
    assert_eq!(absent & (3_u64 << 60), 0);
    assert_eq!(base & (3_u64 << 60), 1_u64 << 60);
    assert_eq!(index & (3_u64 << 60), 1_u64 << 61);
    assert_eq!((base | index) >> 62, 0, "v2 must leave bits 62..63 reserved");
}

#[test]
fn x86_step_form_record_preserves_the_compatible_mixed_table() {
    let source = include_str!("../src/native/engine/backend_tree.c");
    for field in [
        "executed_form_total=%llu",
        "executed_form_unique=%llu",
        "executed_form_overflow=%llu",
        "executed_form%u_key=%llu executed_form%u_count=%llu",
    ] {
        assert!(source.contains(field), "mixed executed-form field disappeared: {field}");
    }
    assert!(source.contains(
        "hl_backend_executed_form_top(census->executed_forms, form_keys, form_counts, HL_BACKEND_EXECUTED_FORM_TOP);"
    ));
    let interpreter = include_str!("../src/native/translator/guest/x86_64/interp.c");
    assert!(interpreter.contains("hl_backend_tree_executed_form(translit_unsupported_key(&insn));"));
}

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
    let finalize = "hl_backend_tree_finalize_from(1, HL_BACKEND_FINALIZE_FATAL_SIGNAL)";
    assert!(tail.contains(finalize), "{tail}");
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
        tail.find(finalize) < tail.find("_exit(128 + sig)"),
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
fn finalizer_provenance_preserves_the_first_and_rejected_second_caller() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 15)
            .unwrap_or_else(|status| panic!("ISA {isa} finalizer provenance scenario failed: {status}"));
    }
}

#[test]
fn restore_style_fork_rebinds_each_process_to_its_own_lifecycle_slot() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 16)
            .unwrap_or_else(|status| panic!("ISA {isa} restore-fork ownership scenario failed: {status}"));
    }
}

#[test]
fn host_guest_pair_reports_whether_translation_codegen_exists() {
    let _serial = TEST_LOCK.lock().unwrap();
    let aarch64_scenario = if cfg!(target_arch = "aarch64") || cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        18
    } else {
        17
    };
    hl_native::backend_tree_census_test(1, aarch64_scenario)
        .unwrap_or_else(|status| panic!("AArch64 guest codegen selection failed: {status}"));
    hl_native::backend_tree_census_test(2, 18)
        .unwrap_or_else(|status| panic!("x86-64 guest codegen selection failed: {status}"));
}

#[test]
fn aarch64_x86_dbt_records_one_typed_exit_per_generated_return() {
    let source = include_str!("../src/native/translator/guest/aarch64/dbt_x86_64.c");
    let body = source
        .split_once("static void run_block(struct cpu *cpu, void *code) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void block_return"))
        .map(|(body, _)| body)
        .expect("AArch64 x86 DBT run_block body");
    let publication = "hl_a64_x86_record_translated_exit((unsigned)header->exit_kind);";
    assert_eq!(body.matches(publication).count(), 1, "{body}");
    let generated = body
        .split_once("} else {")
        .map(|(_, generated)| generated)
        .expect("generated arm");
    assert!(generated.contains(publication), "{generated}");
}

#[test]
fn aarch64_x86_stage_one_keeps_pc_sp_width_and_branch_invariants() {
    let source = include_str!("../src/native/translator/guest/aarch64/dbt_x86_64.c");
    for contract in [
        "base & ~UINT64_C(0xFFF)",
        "(uint64_t)immediate << 12",
        "if (instruction & (1u << 22)) immediate <<= 12;",
        "guest_register == 31u ? OFF_SP",
        "UINT64_C(0xFFFFFFFF)",
        "(instruction & 0xFC000000u) == 0x14000000u",
        "interp_sext(instruction & 0x3FFFFFFu, 26) * 4",
        "cursor + (uint64_t)displacement",
        "guest_pc >= UINT64_MAX - UINT64_C(0xFFF)",
        "count < 64u",
    ] {
        assert!(source.contains(contract), "missing stage-one contract {contract}");
    }
    let fixture = include_str!("../../../../tests/runtime/aarch64-dbt/source/movwide.c");
    for instruction in [
        "adr x2,_start",
        "adrp x0,_start",
        "add sp,x0,#8",
        "sub w0,wsp,#8",
        "b 2f",
        "b 1b",
    ] {
        assert!(fixture.contains(instruction), "fixture omitted {instruction}");
    }
}

#[test]
fn aarch64_x86_unsupported_census_is_observation_gated_and_at_the_rejection_seam() {
    let backend = include_str!("../src/native/engine/backend_tree.c");
    assert!(backend.contains("unsigned form = instruction >> 21;"));
    assert!(backend.contains("if (census == NULL || !census->x86_jcc_route_enabled) return;"));
    assert!(backend.contains("other=%llu"));

    let dbt = include_str!("../src/native/translator/guest/aarch64/dbt_x86_64.c");
    assert_eq!(dbt.matches("hl_backend_tree_a64_unsupported(instruction);").count(), 1);
    assert!(dbt.contains(
        "hl_backend_tree_a64_unsupported(instruction);\n            break;"
    ));
}

#[test]
fn jcc_late_census_bounds_collision_probes_without_losing_in_range_repeats() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 21)
            .unwrap_or_else(|status| panic!("ISA {isa} bounded JCC-late census scenario failed: {status}"));
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
fn jcc_invalid_site_table_counts_duplicates_and_reports_overflow() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 13)
            .unwrap_or_else(|status| panic!("ISA {isa} JCC invalid-site table scenario failed: {status}"));
    }
}

#[test]
fn fork_reservation_gap_is_visible_until_failed_fork_cleanup() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 14)
            .unwrap_or_else(|status| panic!("ISA {isa} backend-tree reservation-gap scenario failed: {status}"));
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
fn executed_form_publication_waits_for_same_and_colliding_keys() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 12)
            .unwrap_or_else(|status| panic!("ISA {isa} concurrent executed-form scenario failed: {status}"));
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
fn sse_riprel_forms_aggregate_after_begin_before_fork_and_reset_between_runs() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 19)
            .unwrap_or_else(|status| panic!("ISA {isa} SSE form fork/reset scenario failed: {status}"));
    }
}

#[test]
fn stranded_sse_riprel_form_reservation_becomes_overflow_without_wedging_survivor() {
    let _serial = TEST_LOCK.lock().unwrap();
    for isa in [1, 2] {
        hl_native::backend_tree_census_test(isa, 20)
            .unwrap_or_else(|status| panic!("ISA {isa} SSE form stranded reservation scenario failed: {status}"));
    }
}

#[test]
fn each_real_translation_stop_site_keeps_its_exact_reason() {
    // The hook scenarios exercise marker transport and exact counter reconciliation. This wiring
    // clamp makes a mutation at any real build-loop assignment red as well: replacing one site by
    // OTHER must not be masked by the sum invariant. FETCH is absent deliberately: translation
    // consumes the decoder's authoritative bytes and has no second fetch that can fail separately.
    let source = include_str!("../src/native/translator/guest/x86_64/translit.inc");
    for assignment in [
        "fall_stop = HL_BACKEND_FALL_DECODE;",
        "fall_stop = kind == TL_SSE2 ? HL_BACKEND_FALL_FS_TO_SSE2 : HL_BACKEND_FALL_FS_TO_NORMAL;",
        "fall_stop = previous_kind == TL_SSE2 ? HL_BACKEND_FALL_SSE2_TO_FS",
        "fall_stop = current_sse2 ? HL_BACKEND_FALL_NORMAL_TO_SSE2",
        "fall_stop = HL_BACKEND_FALL_DISPLACED_UNSAFE;",
        "fall_stop = HL_BACKEND_FALL_RIPREL_LOWER;",
        "fall_stop = HL_BACKEND_FALL_FS_TRANSACTION;",
        "fall_stop = HL_BACKEND_FALL_SSE_RIPREL_LOWER;",
    ] {
        assert_eq!(
            source.matches(assignment).count(),
            1,
            "missing or duplicated real stop site: {assignment}"
        );
    }
    // One site is classifier refusal; the other is the explicit direct-data-authority refusal for
    // an otherwise supported FF /4 encountered after earlier instructions in the same candidate block.
    assert_eq!(source.matches("fall_stop = HL_BACKEND_FALL_TL_NO;").count(), 2);
    // One site preserves a completed prefix after hitting the emitter's body capacity; the other stops
    // an otherwise unterminated block at the architectural instruction-count cap.
    assert_eq!(source.matches("fall_stop = HL_BACKEND_FALL_CAP;").count(), 2);
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
    let gate = source
        .split_once("#define interp_executed_form_complete(cpu, reason)")
        .and_then(|(_, tail)| tail.split_once("#if defined(HL_NATIVE_TEST_HOOKS)"))
        .map(|(body, _)| body)
        .expect("production deferred-completion gate");
    assert!(gate.contains("if (g_dispatch_census_open == 2)"), "{gate}");

    let backend = include_str!("../src/native/engine/backend_tree.c");
    let writer = backend
        .split_once("static inline void hl_backend_tree_executed_form(uint64_t key) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_backend_executed_form_top"))
        .map(|(body, _)| body)
        .expect("product executed-form writer");
    assert!(writer.contains("if (census == NULL || g_backend_mixed_sse_self == NULL) return;"));
}

#[test]
fn product_smc_completion_is_ordered_after_commit_and_before_resume() {
    let dispatcher = include_str!("../src/native/translator/guest/x86_64/interp_dispatch.h");
    let arm = dispatcher
        .split_once("if ((c)->reason == R_SMC)")
        .and_then(|(_, tail)| tail.split_once("if ((c)->reason == R_SYSCALL)"))
        .map(|(body, _)| body)
        .expect("R_SMC dispatcher arm");
    let commit = arm.find("jit86_smc_commit(c)").unwrap();
    let census = arm.find("interp_executed_form_complete(c, R_SMC)").unwrap();
    let resume = arm.find("(c)->reason = R_BRANCH").unwrap();
    assert!(commit < census && census < resume, "{arm}");
}

#[test]
fn x86_aarch64_route_census_commits_once_at_every_translation_outcome() {
    let source = include_str!("../src/native/translator/guest/x86_64/translate.c");
    let loop_body = source
        .split_once("if (hl_x86_decode(gpc, &I) < 0) {")
        .and_then(|(_, tail)| tail.split_once("// IRQSLIM: the out-of-line poll exit stub"))
        .map(|(body, _)| body)
        .expect("x86 AArch64 translation loop");
    assert_eq!(loop_body.matches("hl_x86_a64_route_begin(&I);").count(), 1, "{loop_body}");
    assert_eq!(loop_body.matches("hl_x86_a64_route_commit(0);").count(), 6, "{loop_body}");
    assert_eq!(loop_body.matches("hl_x86_a64_route_commit(1);").count(), 1, "{loop_body}");
    assert!(
        loop_body.find("hl_x86_a64_route_commit(1);").unwrap()
            < loop_body.find("report_unimpl(gpc, &I);").unwrap(),
        "unimplemented attribution must precede the fatal emitter"
    );
}

#[test]
fn x86_aarch64_expansion_census_is_diagnostics_only_and_exactly_reconciled() {
    let source = include_str!("../src/native/translator/guest/x86_64/translate.c");
    let begin = source
        .split_once("static void hl_x86_a64_route_begin(const struct insn *instruction) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_x86_a64_route_note_exit"))
        .map(|(body, _)| body)
        .expect("route begin body");
    assert!(begin.trim_start().starts_with("if (!g_prof) return;"), "{begin}");
    assert!(begin.contains("g_x86_a64_family_emit_begin = (uint32_t *)g_cp;"));

    let commit = source
        .split_once("static void hl_x86_a64_route_commit(int unimplemented) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_x86_a64_route_note_unimplemented"))
        .map(|(body, _)| body)
        .expect("route commit body");
    assert!(commit.trim_start().starts_with("if (!g_prof) return;"), "{commit}");
    assert!(commit.contains("(uint32_t *)g_cp - g_x86_a64_family_emit_begin"));
    assert_eq!(commit.matches("g_x86_a64_family_route_count").count(), 1);
    assert_eq!(commit.matches("g_x86_a64_family_route_words").count(), 1);

    let report = source
        .split_once("static int hl_x86_a64_route_report(char *out, size_t size) {")
        .expect("expansion report")
        .1;
    for family in ["alu", "memory", "branch", "sse", "other"] {
        assert!(report.contains(family), "missing {family}: {report}");
    }
    assert!(report.contains("family_sum == total"));
    assert!(report.contains("family_route_sum[route] == count[route]"));
}

#[test]
fn x86_aarch64_family_classifier_covers_developer_hot_families_before_generic_memory() {
    let source = include_str!("../src/native/translator/guest/x86_64/translate.c");
    let classifier = source
        .split_once("static enum hl_x86_a64_family hl_x86_a64_family(const struct insn *instruction) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_x86_a64_route_begin"))
        .map(|(body, _)| body)
        .expect("family classifier");
    for family in ["FAMILY_INTEGER_ALU", "FAMILY_MEMORY", "FAMILY_BRANCH_CALL", "FAMILY_SSE", "FAMILY_OTHER"] {
        assert!(classifier.contains(family), "missing {family}: {classifier}");
    }
    assert!(classifier.find("FAMILY_SSE").unwrap() < classifier.find("instruction->is_mem").unwrap());
    assert!(classifier.find("FAMILY_BRANCH_CALL").unwrap() < classifier.find("instruction->is_mem").unwrap());
    assert!(classifier.find("op == 0x8d").unwrap() < classifier.find("instruction->is_mem").unwrap());
}

#[test]
fn x86_aarch64_other_subcensus_is_diagnostics_only_and_reconciles_to_other() {
    let source = include_str!("../src/native/translator/guest/x86_64/translate.c");
    let begin = source
        .split_once("static void hl_x86_a64_route_begin(const struct insn *instruction) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_x86_a64_route_note_exit"))
        .map(|(body, _)| body)
        .expect("route begin body");
    assert!(begin.trim_start().starts_with("if (!g_prof) return;"), "{begin}");
    assert!(begin.find("hl_x86_a64_other(instruction)").unwrap() > begin.find("if (!g_prof) return;").unwrap());

    let classifier = source
        .split_once("static enum hl_x86_a64_other hl_x86_a64_other(const struct insn *instruction) {")
        .and_then(|(_, tail)| tail.split_once("static enum hl_x86_a64_family"))
        .map(|(body, _)| body)
        .expect("other classifier");
    for detail in ["OTHER_STACK", "OTHER_MOVE", "OTHER_ADDRESS", "OTHER_SYSTEM", "OTHER_UNKNOWN"] {
        assert!(classifier.contains(detail), "missing {detail}: {classifier}");
    }

    let report = source
        .split_once("[prof] x86-a64-other:")
        .map(|(_, report)| report)
        .expect("other report");
    for field in ["stack=", "move=", "address=", "system=", "unknown=", "reconcile=%u", "words_reconcile=%u"] {
        assert!(report.contains(field), "missing {field}: {report}");
    }
    assert!(report.contains("other_sum == other_family_count"));
    assert!(report.contains("other_words_sum == other_family_words"));
}

#[test]
fn x86_aarch64_route_census_is_diagnostics_gated_and_reconciled() {
    let translator = include_str!("../src/native/translator/guest/x86_64/translate.c");
    let commit = translator
        .split_once("static void hl_x86_a64_route_commit(int unimplemented) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic int hl_x86_a64_route_report"))
        .map(|(body, _)| body)
        .expect("route commit body");
    assert!(commit.trim_start().starts_with("if (!g_prof) return;"), "{commit}");
    for publication in [
        "&g_x86_a64_route_total",
        "&g_x86_a64_route_count[route]",
        "&g_x86_a64_family_route_count[g_x86_a64_family_current][route]",
        "&g_x86_a64_family_route_words[g_x86_a64_family_current][route]",
        "&g_x86_a64_other_count[g_x86_a64_other_current]",
        "&g_x86_a64_other_words[g_x86_a64_other_current]",
    ] {
        assert_eq!(commit.matches(publication).count(), 1, "missing or duplicate publication {publication}: {commit}");
    }
    let other = commit
        .split_once("if (g_x86_a64_family_current == HL_X86_A64_FAMILY_OTHER) {")
        .map(|(_, body)| body)
        .expect("other-family publication guard");
    assert!(other.contains("g_x86_a64_other_count"), "{other}");
    assert!(other.contains("g_x86_a64_other_words"), "{other}");

    let report = translator
        .split_once("static int hl_x86_a64_route_report(char *out, size_t size) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nvoid hl_x86_legacy_jcc_spill"))
        .map(|(body, _)| body)
        .expect("route report body");
    for field in [
        "total=%llu", "direct=%llu", "avx=%llu", "sse3b=%llu", "repstr=%llu", "div=%llu",
        "x87=%llu", "service=%llu", "trap=%llu", "unimpl=%llu", "sum=%llu", "reconcile=%u",
    ] {
        assert!(report.contains(field), "route report omits {field}: {report}");
    }
    assert!(report.contains("total == sum"), "{report}");
}

#[test]
fn x86_aarch64_helper_exit_reasons_remain_route_classified() {
    let source = include_str!("../src/native/translator/guest/x86_64/translate.c");
    let classifier = source
        .split_once("static void hl_x86_a64_route_note_exit(uint64_t reason) {")
        .and_then(|(_, tail)| tail.split_once("\n}\n\nstatic void hl_x86_a64_route_commit"))
        .map(|(body, _)| body)
        .expect("route exit classifier");
    for reason in [
        "R_AVX", "R_SSE3B", "R_REPSTR", "R_DIV", "R_IDIV", "R_X87FLD", "R_X87FSTP",
        "R_X87FUNC", "R_X87ENV", "R_CPUID", "R_CMPXCHG16", "R_FXSAVE", "R_FXRSTOR", "R_XSAVE",
        "R_RCL", "R_SYSCALL", "R_TRAP",
    ] {
        assert_eq!(classifier.matches(reason).count(), 1, "missing or duplicated {reason}: {classifier}");
    }
    let emitter = include_str!("../src/native/translator/guest/x86_64/emit.c");
    assert_eq!(emitter.matches("hl_x86_a64_route_note_exit(reason);").count(), 1);
    let reporter = source
        .split_once("void report_unimpl(uint64_t pc, struct insn *I) {")
        .map(|(_, body)| body)
        .expect("unimplemented emitter");
    assert!(reporter.trim_start().starts_with("hl_x86_a64_route_note_unimplemented();"));
}
