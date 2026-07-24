use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{IgnoredResult, Rule, Workspace};

fn findings(source: &str) -> Vec<crate::Finding> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "hl-design-ignored-result-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let path: PathBuf = root.join("src/lib.rs");
    fs::write(&path, source).unwrap();
    let workspace = Workspace::load([path]).unwrap();
    let findings = IgnoredResult.check(&workspace).unwrap();
    fs::remove_dir_all(root).unwrap();
    findings
}

#[test]
fn reports_each_proven_discard_form_including_closure_blocks() {
    let values = findings(
        r#"
fn fail() -> Result<(), Error> { todo!() }
fn run() {
    let _ = fail();
    fail();
    drop(fail());
    fail().ok();
    let closure = || { fail(); };
}
"#,
    );
    assert_eq!(values.len(), 5);
    assert!(values.iter().all(crate::Finding::is_violation));
}

#[test]
fn reports_unambiguous_result_methods_and_awaits() {
    let values = findings(
        r#"
struct Store;
impl Store {
    fn save(&self) -> std::io::Result<()> { todo!() }
    fn run(&self) {
        self.save();
        Store::save(self);
    }
}
async fn run() {
    async_result().await;
}
async fn async_result() -> Result<(), Error> { todo!() }
"#,
    );
    assert_eq!(values.len(), 3);
}

#[test]
fn explicit_result_constructors_need_no_declaration() {
    let values = findings(
        r#"
fn run() {
    Result::Err::<(), Error>(error());
    let _ = Result::Ok::<(), Error>(());
}
"#,
    );
    assert_eq!(values.len(), 2);
}

#[test]
fn ignores_handled_results_and_option_discards() {
    let values = findings(
        r#"
fn fail() -> Result<(), Error> { todo!() }
fn maybe() -> Option<()> { None }
fn run() -> Result<(), Error> {
    fail()?;
    match fail() { Ok(()) => {}, Err(error) => log(error) }
    if let Err(error) = fail() { log(error); }
    let result = fail();
    let _ = maybe();
    maybe();
    drop(maybe());
    Ok(())
}
"#,
    );
    assert!(values.is_empty());
}

#[test]
fn ambiguous_names_are_not_syntactic_proof() {
    let values = findings(
        r#"
fn operation() -> Result<(), Error> { todo!() }
mod other { fn operation() {} }
struct First;
impl First { fn save(&self) -> Result<(), Error> { todo!() } }
struct Second;
impl Second { fn save(&self) {} }
fn run(first: First) {
    operation();
    first.save();
    unknown_but_fallible_sounding();
}
"#,
    );
    assert!(values.is_empty());
}

#[test]
fn excludes_test_modules_and_functions() {
    let values = findings(
        r#"
fn fail() -> Result<(), Error> { todo!() }
#[cfg(test)]
mod tests { fn case() { fail(); } }
#[test]
fn test_function() { fail(); }
"#,
    );
    assert!(values.is_empty());
}
