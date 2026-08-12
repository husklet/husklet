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
    let findings = analyze(Path::new("large.c"), &source);
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
    assert!(analyze(Path::new("small.c"), source).is_empty());
}
