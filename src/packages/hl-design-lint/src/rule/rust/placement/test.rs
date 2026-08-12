use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{rule::Rule, source::Workspace};

use super::IntegrationCandidate;

fn check(name: &str, test: &str) -> usize {
    check_with_library(
        name,
        "pub struct Engine;\nimpl Engine { pub fn run(&self) {} }\nfn private() {}\n#[cfg(test)] mod behavior_test;\n",
        test,
    )
}

fn check_with_library(name: &str, library: &str, test: &str) -> usize {
    let root = std::env::temp_dir().join(format!(
        "hl-design-placement-{name}-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.0.0'\n").unwrap();
    fs::write(root.join("src/lib.rs"), library).unwrap();
    fs::write(root.join("src/behavior_test.rs"), test).unwrap();
    let workspace = Workspace::load([root.clone()]).unwrap();
    let count = IntegrationCandidate.check(&workspace).unwrap().len();
    fs::remove_dir_all(root).unwrap();
    count
}

#[test]
fn reports_public_api_only_candidate() {
    assert_eq!(
        check("public", "use crate::Engine;\n#[test] fn runs() { Engine.run(); }\n"),
        1
    );
}

#[test]
fn reports_public_module_api_candidate() {
    assert_eq!(
        check_with_library(
            "public-module",
            "pub mod api { pub struct Engine; }\n#[cfg(test)] mod behavior_test;\n",
            "use crate::api::Engine;\n#[test] fn runs() { let _ = Engine; }\n",
        ),
        1,
    );
}

#[test]
fn keeps_super_dependency_as_unit_test() {
    assert_eq!(
        check("super", "use super::private;\n#[test] fn runs() { private(); }\n"),
        0
    );
}

#[test]
fn keeps_private_crate_item_as_unit_test() {
    assert_eq!(
        check("private", "use crate::private;\n#[test] fn runs() { private(); }\n"),
        0
    );
}

#[test]
fn keeps_private_field_sensitive_test() {
    assert_eq!(
        check(
            "field",
            "use crate::Engine;\n#[test] fn runs() { let value = Engine; value.hidden; }\n"
        ),
        0
    );
}

#[test]
fn keeps_pub_crate_fixture_as_unit_test() {
    assert_eq!(
        check(
            "restricted",
            "pub(crate) struct Fixture;\n#[test] fn runs() { let _ = Fixture; }\n"
        ),
        0
    );
}
