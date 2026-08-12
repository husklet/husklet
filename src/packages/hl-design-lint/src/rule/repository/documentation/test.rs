use super::validate_example;
use std::fs;

#[test]
fn example_contract_rejects_missing_cases_and_open_fences() {
    let path = std::env::temp_dir().join(format!("design-example-{}.md", std::process::id()));
    fs::write(&path, "# Example\n\n```rust\nfn open() {}\n").unwrap();
    let mut findings = Vec::new();
    validate_example(&path, &mut findings).unwrap();
    assert_eq!(findings.len(), 2);
    fs::remove_file(path).unwrap();
}

#[test]
fn example_contract_accepts_a_titled_case() {
    let path = std::env::temp_dir().join(format!("design-example-good-{}.md", std::process::id()));
    fs::write(&path, "# Examples\n\n## One case\n\n```rust\nfn closed() {}\n```\n").unwrap();
    let mut findings = Vec::new();
    validate_example(&path, &mut findings).unwrap();
    assert!(findings.is_empty());
    fs::remove_file(path).unwrap();
}
