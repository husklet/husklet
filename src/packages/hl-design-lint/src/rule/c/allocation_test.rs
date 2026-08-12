use super::analyze;
use std::{collections::BTreeSet, path::Path};

fn findings(source: &str) -> Vec<crate::Finding> {
    analyze(
        Path::new("allocation.c"),
        source,
        &BTreeSet::from(["allocate".to_owned()]),
    )
    .unwrap()
}

#[test]
fn reports_dereference_without_null_check() {
    let values = findings("void run(void) { int *value = allocate(4); *value = 1; }");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].rule, "c-unchecked-allocation");
}

#[test]
fn accepts_common_null_checks_and_unused_allocation() {
    assert!(findings("void run(void) { int *value = allocate(4); if (!value) return; value[0] = 1; }").is_empty());
    assert!(findings("void run(void) { int *value = allocate(4); consume(value); }").is_empty());
}

#[test]
fn accepts_null_checks_composed_with_other_failure_conditions() {
    let source =
        "void run(void) { int *value = allocate(4); if (!value || initialize(value) != 0) return; value[0] = 1; }";
    assert!(findings(source).is_empty());
    let source = "void run(int requested) { int *value = allocate(4); if (requested && !value) return; value[0] = 1; }";
    assert!(findings(source).is_empty());
}

#[test]
fn reasoned_suppression_is_exact() {
    let source = "void run(void) {\n// hl-lint: allow(c-unchecked-allocation) -- allocator aborts on exhaustion\nint *value = allocate(4);\n*value = 1;\n}";
    assert!(findings(source).is_empty());
}

#[test]
fn stale_suppression_is_reported() {
    let source = "void run(void) {\n// hl-lint: allow(c-unchecked-allocation) -- allocator aborts on exhaustion\nint *value = allocate(4);\nif (!value) return;\n*value = 1;\n}";
    assert!(
        findings(source)
            .iter()
            .any(|finding| finding.rule == "c-lint-suppression")
    );
}
