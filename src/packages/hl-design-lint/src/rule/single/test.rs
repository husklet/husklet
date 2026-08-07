use std::{fs, path::PathBuf, time::SystemTime};

use crate::rule::Rule;

use super::Use;

fn findings(source: &str) -> Vec<crate::Finding> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("hl-single-rule-{nonce}"));
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
    let values = Use.check(&workspace).expect("run rule");
    fs::remove_dir_all(root).expect("remove fixture");
    values
}

fn subjects(source: &str) -> Vec<String> {
    let mut names = findings(source)
        .into_iter()
        .map(|finding| finding.subject)
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
fn an_attribute_only_use_is_excluded() {
    assert!(
        subjects(
            r#"
#[derive(serde::Deserialize)]
struct Options {
    #[serde(default = "fallback")]
    value: u16,
    #[arg(value_parser = socket)]
    path: String,
    #[arg(default_value_t = platform())]
    platform: u8,
    #[cfg_attr(unix, serde(deserialize_with = "gated"))]
    flag: bool,
}
fn fallback() -> u16 {
    7
}
fn socket(value: &str) -> Result<String, String> {
    Ok(value.to_owned())
}
fn platform() -> u8 {
    1
}
fn gated() -> bool {
    true
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn an_ordinary_sole_use_still_fires() {
    assert_eq!(
        subjects(
            r"
fn fallback() -> u16 {
    7
}
fn build() -> u16 {
    fallback()
}
",
        ),
        ["fallback"]
    );
}

#[test]
fn attribute_reference_is_a_second_use() {
    assert!(
        subjects(
            r#"
struct Options {
    #[serde(default = "fallback")]
    value: u16,
}
fn fallback() -> u16 {
    7
}
fn build() -> u16 {
    fallback()
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn a_test_module_use_is_not_a_production_use() {
    assert_eq!(
        subjects(
            r"
fn fallback() -> u16 {
    7
}
fn build() -> u16 {
    fallback()
}
#[cfg(test)]
mod tests {
    #[test]
    fn covers() {
        let value = super::fallback();
        let _ = value;
    }
}
",
        ),
        ["fallback"]
    );
}

#[test]
fn a_test_gated_function_does_not_lend_a_production_use() {
    assert_eq!(
        subjects(
            r"
fn fallback() -> u16 {
    7
}
fn build() -> u16 {
    fallback()
}
#[cfg(test)]
fn helper() -> u16 {
    fallback()
}
",
        ),
        ["fallback"]
    );
}

#[test]
fn a_function_used_only_from_tests_is_not_reported() {
    assert!(
        subjects(
            r"
fn fallback() -> u16 {
    7
}
#[cfg(test)]
mod tests {
    #[test]
    fn covers() {
        let value = super::fallback();
        let _ = value;
    }
}
",
        )
        .is_empty()
    );
}

#[test]
fn a_macro_body_call_is_a_use() {
    assert_eq!(
        subjects(
            r#"
fn fallback() -> u16 {
    7
}
fn build() -> String {
    format!("{}", fallback())
}
"#,
        ),
        ["fallback"]
    );
}

#[test]
fn a_macro_body_call_is_a_second_use() {
    assert!(
        subjects(
            r#"
fn fallback() -> u16 {
    7
}
fn build() -> String {
    let first = fallback();
    format!("{first}{}", fallback())
}
"#,
        )
        .is_empty()
    );
}

#[test]
fn a_method_inside_a_macro_body_is_not_a_use() {
    assert_eq!(
        subjects(
            r#"
fn fallback() -> u16 {
    7
}
fn build(value: &Holder) -> String {
    format!("{}{}", value.fallback(), fallback())
}
"#,
        ),
        ["fallback"]
    );
}

#[test]
fn a_macro_definition_body_is_not_a_use() {
    assert_eq!(
        subjects(
            r"
fn fallback() -> u16 {
    7
}
macro_rules! shorthand {
    () => {
        fallback()
    };
}
fn build() -> u16 {
    fallback()
}
",
        ),
        ["fallback"]
    );
}

#[test]
fn a_macro_body_call_inside_a_test_module_is_not_a_production_use() {
    assert_eq!(
        subjects(
            r"
fn fallback() -> u16 {
    7
}
fn build() -> u16 {
    fallback()
}
#[cfg(test)]
mod tests {
    #[test]
    fn covers() {
        assert_eq!(super::fallback(), 7);
    }
}
",
        ),
        ["fallback"]
    );
}

#[test]
fn attribute_words_are_not_references() {
    assert_eq!(
        subjects(
            r#"
struct Options {
    #[arg(long, short, long = "helper", value_name = "helper")]
    #[serde(default, rename = "helper", alias = "helper")]
    #[cfg(target_os = "helper")]
    value: u16,
}
fn helper() -> u16 {
    1
}
fn build() -> u16 {
    helper()
}
"#,
        ),
        ["helper"]
    );
}
