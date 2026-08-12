use super::{FILE_LINES, FUNCTION_LINES, analyze};
use std::path::Path;

#[test]
fn reports_each_c_structure_limit() {
    let mut source = String::from("int oversized(void) {\n");
    for _ in 0..=FUNCTION_LINES {
        source.push_str("  value += 1;\n");
    }
    for _ in 0..8 {
        source.push_str("  if (value) {\n");
    }
    for _ in 0..8 {
        source.push_str("  }\n");
    }
    source.push_str("}\n");
    for _ in source.lines().count()..=FILE_LINES {
        source.push_str("int declaration;\n");
    }
    let findings = analyze(Path::new("large.c"), &source).unwrap();
    for subject in ["file length", "function length", "function nesting"] {
        assert!(
            findings.iter().any(|finding| finding.subject == subject),
            "missing {subject}"
        );
    }
}

#[test]
fn comments_strings_and_prototypes_do_not_forge_structure() {
    let source = r#"
/* int fake(void) { {{{{{{{ */
const char *text = "int fake(void) { {{{{{{{";
int prototype(void);
int small(void) { return 0; }
"#;
    assert!(analyze(Path::new("small.c"), source).unwrap().is_empty());
}

#[test]
fn embedded_parser_handles_preprocessor_and_counts_control_flow_not_braces() {
    let source = r"
#define WRAP(value) do { value; } while (0)
int sample(int value) {
    struct Local { int nested[8]; } local = {0};
    if (value) { while (value) { for (;;) { switch (value) { default: do { value--; } while (value); } } } }
    return local.nested[0];
}
";
    let findings = analyze(Path::new("portable.c"), source).unwrap();
    assert!(findings.iter().all(|finding| finding.rule != "c-maximum-nesting"));
}

#[test]
fn else_if_chain_is_one_decision_level() {
    let source = r"
int classify(int value) {
    if (value == 1) return 1;
    else if (value == 2) return 2;
    else if (value == 3) return 3;
    else if (value == 4) return 4;
    else if (value == 5) return 5;
    else if (value == 6) return 6;
    else if (value == 7) return 7;
    else return 0;
}
";
    let findings = analyze(Path::new("classification.c"), source).unwrap();
    assert!(findings.iter().all(|finding| finding.rule != "c-maximum-nesting"));
}

#[test]
fn reasoned_annotation_suppresses_exactly_one_structural_diagnostic() {
    let mut source = String::from(
        "// hl-lint: allow(c-function-length) -- generated dispatch table is algorithmic data\nint generated(void) {\n",
    );
    for _ in 0..=FUNCTION_LINES {
        source.push_str("  value += 1;\n");
    }
    source.push_str("  return value;\n}\n");
    let findings = analyze(Path::new("generated.c"), &source).unwrap();
    assert!(findings.iter().all(|finding| finding.rule != "c-function-length"));
}

#[test]
fn obsolete_structural_annotation_is_a_lint_error() {
    let source = "// hl-lint: allow(c-function-length) -- no longer large\nint small(void) { return 0; }\n";
    let findings = analyze(Path::new("small.c"), source).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.rule == "c-lint-suppression" && finding.message.contains("unnecessary"))
    );
}
