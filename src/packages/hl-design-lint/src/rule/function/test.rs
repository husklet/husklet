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
fn flag(deserializer: u8) -> bool {
    deserializer == 0
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
fn flag(deserializer: u8) -> bool {
    deserializer == 0
}
"#,
    );
    let [finding] = &values[..] else {
        panic!("one candidate, got {}", values.len());
    };
    assert!(finding.related.is_empty());
}
