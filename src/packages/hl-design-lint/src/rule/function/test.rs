use std::{fs, path::PathBuf, time::SystemTime};

use crate::rule::Rule;

use super::FreeFunction;

fn findings(source: &str) -> Vec<crate::Finding> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("hl-function-rule-{nonce}"));
    let package = root.join("src/packages/fixture");
    let path = package.join("src/lib.rs");
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture");
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write manifest");
    fs::write(&path, source).expect("write fixture");
    let workspace = crate::source::Workspace::load([PathBuf::from(&path)]).expect("parse fixture");
    let values = FreeFunction.check(&workspace).expect("run rule");
    fs::remove_dir_all(root).expect("remove fixture");
    values
}

#[test]
fn attribute_references_become_related_context() {
    let values = findings(
        r#"
struct Options {
    #[serde(deserialize_with = "crate::flag")]
    first: bool,
    #[serde(deserialize_with = "crate::flag")]
    second: bool,
}
struct Flags;
fn flag(flags: Flags) -> bool {
    matches!(flags, Flags)
}
"#,
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert_eq!(finding.subject, "flag");
    assert_eq!(finding.related.len(), 2);
}

#[test]
fn attribute_words_are_not_related_context() {
    let values = findings(
        r#"
struct Options {
    #[arg(long = "flag", value_name = "flag")]
    #[serde(rename = "flag")]
    first: bool,
}
struct Flags;
fn flag(flags: Flags) -> bool {
    matches!(flags, Flags)
}
"#,
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert!(finding.related.is_empty());
}

#[test]
fn a_local_binding_is_not_related_context() {
    let values = findings(
        r"
struct Flags;
fn flag(value: Flags) -> bool {
    matches!(value, Flags)
}
fn read() -> bool {
    flag(Flags)
}
fn shadow() -> u8 {
    let flag = 2;
    flag
}
",
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert_eq!(finding.subject, "flag");
    assert_eq!(finding.related.len(), 1);
}

#[test]
fn a_function_over_only_foreign_types_has_no_receiver_to_become() {
    let values = findings(
        r"
fn excerpt(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
fn ratio(value: u64, reference: u64) -> f64 {
    value as f64 / reference as f64
}
",
    );
    assert!(values.is_empty(), "got {values:?}");
}

#[test]
fn a_declared_type_in_any_argument_position_keeps_the_candidate() {
    let values = findings(
        r"
pub enum Verdict { Pass }
fn render(limit: usize, verdict: &Verdict) -> usize {
    match verdict { Verdict::Pass => limit }
}
",
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert_eq!(finding.subject, "render");
}

#[test]
fn a_declared_type_nested_in_a_generic_keeps_the_candidate() {
    let values = findings(
        r"
pub struct Case;
fn plan(cases: Vec<Case>, limit: usize) -> usize {
    cases.len() + limit
}
",
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert_eq!(finding.subject, "plan");
}
