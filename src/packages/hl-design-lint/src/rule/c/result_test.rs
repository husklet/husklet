use std::{collections::BTreeSet, path::Path};

use super::analyze;

fn findings(source: &str) -> Vec<crate::Finding> {
    analyze(
        Path::new("resource.c"),
        source,
        &BTreeSet::from(["open_resource".to_owned()]),
    )
    .unwrap()
}

#[test]
fn reports_a_configured_direct_call_used_as_a_statement() {
    let values = findings("void run(void) { open_resource(); }");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].rule, "c-ignored-result");
}

#[test]
fn parentheses_do_not_hide_a_discarded_result() {
    let values = findings("void run(void) { (((open_resource()))); }");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "open_resource");
}

#[test]
fn parentheses_around_function_designator_do_not_hide_a_discarded_result() {
    let values = findings("void run(void) { (((open_resource)))(); }");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "open_resource");
}

#[test]
fn non_void_cast_does_not_hide_a_discarded_result() {
    let values = findings("void run(void) { (long)(((open_resource)))(); }");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].subject, "open_resource");
}

#[test]
fn accepts_consumed_returned_and_explicitly_discarded_results() {
    let source = "int run(void) { int value = open_resource(); if (value < 0) return value; return open_resource(); }\nvoid probe(void) { (void)open_resource(); }";
    assert!(findings(source).is_empty());
}

#[test]
fn reasoned_annotation_suppresses_one_ignored_result() {
    let source = "void run(void) {\n// hl-lint: allow(c-ignored-result) -- best-effort cleanup\nopen_resource();\n}";
    assert!(findings(source).is_empty());
}

#[test]
fn stale_annotation_is_reported() {
    let source =
        "void run(void) {\n// hl-lint: allow(c-ignored-result) -- best-effort cleanup\nint value = open_resource();\n}";
    assert!(
        findings(source)
            .iter()
            .any(|finding| finding.rule == "c-lint-suppression")
    );
}
