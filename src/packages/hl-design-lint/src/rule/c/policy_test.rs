use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{Rule, source::Workspace};

use super::{CallPolicy, Policy};

fn findings(source: &str, policy: Policy) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-c-policy-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("arbitrary-source-root")).unwrap();
    let path = root.join("arbitrary-source-root/arbitrary-name.c");
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([PathBuf::from(&root)]).unwrap();
    let result = policy.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    result
}

fn shell() -> Policy {
    Policy::new().forbid_calls(CallPolicy::new(
        "shell-execution",
        ["system", "popen"],
        "shell execution",
        "launch an explicit argv vector",
    ))
}

#[test]
fn embedded_parser_finds_real_calls_with_exact_locations() {
    let result = findings("int f(void) { return system(\"x\"); }\n", shell());
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].rule, "shell-execution");
    assert_eq!((result[0].location.line, result[0].location.column), (1, 22));
    assert_eq!(result[0].location.source, "system");
}

#[test]
fn strings_comments_members_and_larger_identifiers_are_not_calls() {
    let result = findings(
        "// system(\"x\")\nconst char *s = \"popen(x)\";\nint f(void) { object.system(); return subsystem(); }\n",
        shell(),
    );
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn annotation_spelling_inside_a_string_is_not_a_directive() {
    let result = findings(
        "const char *s = \"hl-lint: allow(shell-execution) -- forged\";\nint f(void) { return system(\"x\"); }\n",
        shell(),
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].rule, "shell-execution");
}

#[test]
fn standard_environment_access_is_allowed_by_default() {
    assert!(findings("char *f(void) { return getenv(\"HOME\"); }\n", shell()).is_empty());
}

#[test]
fn caller_can_prohibit_environment_access_explicitly() {
    let policy = Policy::new().forbid_calls(CallPolicy::new(
        "ambient-environment",
        ["getenv"],
        "ambient environment",
        "inject configuration",
    ));
    assert_eq!(
        findings("char *f(void) { return getenv(\"HOME\"); }\n", policy)[0].rule,
        "ambient-environment"
    );
}

#[test]
fn one_reasoned_annotation_suppresses_one_finding_on_next_code_line() {
    let result = findings(
        "// hl-lint: allow(shell-execution) -- compatibility launcher has no argv API\nint f(void) { return system(\"x\"); }\n",
        shell(),
    );
    assert!(result.is_empty(), "{result:#?}");
}

#[test]
fn suppression_validation_rejects_unknown_malformed_stale_and_overbroad() {
    for (source, message) in [
        (
            "// hl-lint: allow(no-such-rule) -- reason\nint f(void) { return 0; }\n",
            "unknown suppression rule",
        ),
        (
            "// hl-lint: allow(shell-execution)\nint f(void) { return system(\"x\"); }\n",
            "malformed suppression",
        ),
        (
            "// hl-lint: allow(shell-execution) -- obsolete\nint f(void) { return 0; }\n",
            "stale or unnecessary suppression",
        ),
        (
            "// hl-lint: allow(shell-execution) -- two calls\nint f(void) { return system(\"x\") + system(\"y\"); }\n",
            "overbroad suppression",
        ),
    ] {
        let result = findings(source, shell());
        assert!(
            result.iter().any(|finding| finding.message == message),
            "missing {message}: {result:#?}"
        );
    }
}
