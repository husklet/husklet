use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Rule, policy::CTestOnlyStatePolicy, source::Workspace};

use super::TestOnlyState;

fn findings(files: &[(&str, &str)]) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-c-test-only-state-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("arbitrary-source-root")).unwrap();
    for (name, source) in files {
        fs::write(root.join("arbitrary-source-root").join(name), source).unwrap();
    }
    let workspace = Workspace::load([PathBuf::from(&root)]).unwrap();
    let rule = TestOnlyState::new(CTestOnlyStatePolicy {
        macros: vec!["HL_NATIVE_TEST_HOOKS".to_owned()],
    });
    let result = rule.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    result
}

const WRITER: &str = "\
static int g_member_bound;
void admit(void) { g_member_bound = 1; }
";

#[test]
fn production_predicate_on_state_written_only_behind_a_test_hook_is_reported() {
    let result = findings(&[
        (
            "writer.c",
            &format!("{WRITER}\n#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) {{ admit(); }}\n#endif\n"),
        ),
        (
            "dump.c",
            "extern int g_member_bound;\nint dump(void) { if (!g_member_bound) { return -1; } return 0; }\n",
        ),
    ]);
    assert_eq!(result.len(), 1, "{result:#?}");
    assert_eq!(result[0].rule, "c-test-only-state");
    assert_eq!(result[0].subject, "g_member_bound");
    assert_eq!(result[0].location.line, 2);
    assert!(result[0].related.iter().any(|related| related.label.contains("admit")));
}

#[test]
fn one_production_call_of_the_writer_clears_the_finding() {
    let result = findings(&[
        (
            "writer.c",
            &format!(
                "{WRITER}\nvoid boot(void) {{ admit(); }}\n#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) {{ admit(); }}\n#endif\n"
            ),
        ),
        (
            "dump.c",
            "extern int g_member_bound;\nint dump(void) { if (!g_member_bound) { return -1; } return 0; }\n",
        ),
    ]);
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn a_writer_reached_only_through_a_test_only_caller_chain_is_still_test_only() {
    let result = findings(&[(
        "chain.c",
        &format!(
            "{WRITER}\nvoid stage(void) {{ admit(); }}\n#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) {{ stage(); }}\n#endif\n\
             int dump(void) {{ return g_member_bound ? 0 : -1; }}\n"
        ),
    )]);
    assert_eq!(result.len(), 1, "{result:#?}");
    assert_eq!(result[0].subject, "g_member_bound");
}

#[test]
fn test_only_predicates_and_test_only_symbols_alone_are_not_reported() {
    let result = findings(&[(
        "hooks.c",
        "static int g_probe;\n#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) { g_probe = 1; }\n\
         int observe(void) { if (!g_probe) { return -1; } return 0; }\n#endif\n",
    )]);
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn the_production_branch_of_a_test_hook_conditional_is_production() {
    let result = findings(&[(
        "either.c",
        "static int g_ready;\n#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) { g_ready = 2; }\n\
         #else\nvoid arm(void) { g_ready = 1; }\n#endif\nint use(void) { return g_ready == 1; }\n",
    )]);
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn a_disjunction_that_holds_without_the_test_macro_is_not_test_only() {
    let result = findings(&[(
        "either.c",
        "static int g_ready;\n#if defined(HL_NATIVE_TEST_HOOKS) || defined(HL_DIAGNOSTICS)\n\
         void arm(void) { g_ready = 1; }\n#endif\nint use(void) { return g_ready == 1; }\n",
    )]);
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn state_carrying_a_real_production_initializer_is_not_unwritten() {
    let result = findings(&[(
        "seeded.c",
        "static int g_ready = 1;\n#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) { g_ready = 0; }\n#endif\n\
         int use(void) { if (!g_ready) { return -1; } return 0; }\n",
    )]);
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn a_function_with_no_call_site_is_a_production_entry_point() {
    let result = findings(&[(
        "exported.c",
        "static int g_ready;\nvoid hl_admit(void) { g_ready = 1; }\nint use(void) { return g_ready == 1; }\n",
    )]);
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn local_state_that_is_not_file_scope_is_not_tracked() {
    let result = findings(&[(
        "local.c",
        "#if defined(HL_NATIVE_TEST_HOOKS)\nvoid arm(void) { int ready = 1; (void)ready; }\n#endif\n\
         int use(void) { int ready = 0; return ready == 1; }\n",
    )]);
    assert!(result.is_empty(), "{result:#?}");
}
