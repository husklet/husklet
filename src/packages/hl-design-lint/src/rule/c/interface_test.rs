use super::analyze;
use std::path::Path;

#[test]
fn reports_headers_beyond_the_configured_interface_limit() {
    let source = "int open_store(void);\nint read_store(void);\nint close_store(void);\n";
    let findings = analyze(Path::new("store.h"), source, 2).unwrap();
    assert_eq!(
        findings
            .iter()
            .filter(|finding| finding.rule == "c-interface-breadth")
            .count(),
        1
    );
    assert!(findings[0].message.contains("3 function declarations"));
}

#[test]
fn ignores_static_helpers() {
    let source = "static int helper(void);\nint public_api(void);\n";
    assert!(analyze(Path::new("api.h"), source, 1).unwrap().is_empty());
}

#[test]
fn reasoned_annotation_suppresses_exactly_one_broad_interface() {
    let source = "// hl-lint: allow(c-interface-breadth) -- generated protocol surface\nint a(void);\nint b(void);\n";
    assert!(analyze(Path::new("protocol.h"), source, 1).unwrap().is_empty());
}

#[test]
fn stale_interface_annotation_is_reported() {
    let source = "// hl-lint: allow(c-interface-breadth) -- generated protocol surface\nint a(void);\n";
    let findings = analyze(Path::new("protocol.h"), source, 1).unwrap();
    assert!(findings.iter().any(|finding| finding.rule == "c-lint-suppression"));
}
