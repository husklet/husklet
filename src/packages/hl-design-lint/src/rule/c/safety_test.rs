use std::{collections::BTreeSet, path::Path};

use super::analyze;

fn findings(source: &str) -> Vec<crate::Finding> {
    analyze(
        Path::new("memory.c"),
        source,
        &BTreeSet::from(["copy_bytes".to_owned()]),
    )
    .unwrap()
}

#[test]
fn reports_configured_operation_without_a_rationale() {
    let values = findings("void run(void) {\ncopy_bytes();\n}");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].rule, "c-safety-rationale");
}

#[test]
fn accepts_an_attached_nonempty_safety_rationale() {
    let source = "void run(void) {\n// SAFETY: destination owns eight writable bytes and source owns eight readable bytes.\ncopy_bytes();\n}";
    assert!(findings(source).is_empty());
}

#[test]
fn detached_or_empty_rationale_does_not_justify_the_call() {
    let detached = "void run(void) {\n// SAFETY: buffers are valid.\n\ncopy_bytes();\n}";
    let empty = "void run(void) {\n// SAFETY:\ncopy_bytes();\n}";
    assert_eq!(findings(detached).len(), 1);
    assert_eq!(findings(empty).len(), 1);
}

#[test]
fn reasoned_suppression_is_exact_and_stale_suppression_is_reported() {
    let allowed = "void run(void) {\n// hl-lint: allow(c-safety-rationale) -- generated bounds proof\ncopy_bytes();\n}";
    assert!(findings(allowed).is_empty());
    let stale = "void run(void) {\n// hl-lint: allow(c-safety-rationale) -- generated bounds proof\nreturn;\n}";
    assert!(stale_findings(stale));
}

fn stale_findings(source: &str) -> bool {
    findings(source)
        .iter()
        .any(|finding| finding.rule == "c-lint-suppression")
}
