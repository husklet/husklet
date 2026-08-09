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
fn a_second_argument_relates_two_things_rather_than_naming_a_receiver() {
    let values = findings(
        r"
pub enum Verdict { Pass }
fn render(limit: usize, verdict: &Verdict) -> usize {
    match verdict { Verdict::Pass => limit }
}
",
    );
    assert!(values.is_empty(), "got {values:?}");
}

#[test]
fn a_collected_argument_is_a_transformation_with_no_receiver() {
    let values = findings(
        r"
pub struct Case;
fn plan(cases: Vec<Case>) -> usize {
    cases.len()
}
fn count(cases: &[Case]) -> usize {
    cases.len()
}
fn first(case: Option<Case>) -> bool {
    case.is_some()
}
",
    );
    assert!(values.is_empty(), "got {values:?}");
}

#[test]
fn a_sole_declared_argument_is_the_receiver_the_method_form_takes() {
    let values = findings(
        r"
pub struct Build;
fn validate_build(build: &Build) -> bool {
    let _ = build;
    true
}
",
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert_eq!(finding.subject, "validate_build");
}

#[test]
fn a_foreign_type_sharing_a_declared_name_is_not_this_tree_s_type() {
    let values = findings(
        r"
use std::path::Path;
pub struct Path;
fn portable_name(path: &Path) -> bool {
    path.is_absolute()
}
fn build_id(path: &std::path::Path) -> bool {
    path.is_absolute()
}
",
    );
    assert!(values.is_empty(), "got {values:?}");
}

#[test]
fn a_command_line_argument_type_is_a_boundary_value_not_an_entity() {
    let values = findings(
        r"
#[derive(clap::Args)]
pub struct Options {
    pub verbose: bool,
}
pub fn run(options: Options) -> bool {
    options.verbose
}
",
    );
    assert!(values.is_empty(), "got {values:?}");
}
