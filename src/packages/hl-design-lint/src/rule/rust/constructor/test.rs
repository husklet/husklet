use std::{fs, time::SystemTime};

use crate::rule::Rule;

use super::Ownership;

fn findings(source: &str) -> Vec<crate::Finding> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("hl-constructor-rule-{nonce}"));
    let package = root.join("src/packages/fixture");
    let path = package.join("src/lib.rs");
    fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create fixture");
    fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
    )
    .expect("write manifest");
    fs::write(&path, source).expect("write fixture");
    let workspace = crate::source::Workspace::load([path]).expect("parse fixture");
    let values = Ownership.check(&workspace).expect("run rule");
    fs::remove_dir_all(root).expect("remove fixture");
    values
}

#[test]
fn detached_concrete_and_wrapped_constructors_are_reported() {
    let values = findings(
        r#"
struct Lease { value: usize }
struct Token(usize);
fn open() -> Result<Lease, Error> { Ok(Lease { value: 1 }) }
fn token() -> Option<Token> { Some(Token(1)) }
struct Error;
"#,
    );
    assert_eq!(
        values
            .iter()
            .map(|finding| finding.subject.as_str())
            .collect::<Vec<_>>(),
        ["open", "token"]
    );
    assert!(values[0].help.contains("Lease::open"));
}

#[test]
fn associated_constructor_returning_self_is_already_owned() {
    let values = findings(
        r#"
struct Leases;
impl Leases {
    fn open() -> Result<Self, Error> { Ok(Self) }
}
struct Error;
"#,
    );
    assert!(values.is_empty(), "got {values:?}");
}

#[test]
fn uncertain_ownership_is_not_reported() {
    let values = findings(
        r#"
struct Local;
struct Other;
fn generic<T>() -> T { todo!() }
fn dynamic() -> Box<dyn Send> { todo!() }
fn opaque() -> impl Send { Local }
fn forwarded() -> Local { other::make() }
fn orchestrated() -> Local { let _other = Other; Local }
fn converted(value: usize) -> Local { Local::from(value) }
mod other { pub(super) fn make() -> super::Local { super::Local } }
impl From<usize> for Local { fn from(_: usize) -> Self { Self } }
"#,
    );
    assert_eq!(
        values
            .iter()
            .map(|finding| finding.subject.as_str())
            .collect::<Vec<_>>(),
        ["orchestrated", "make"]
    );
}

#[test]
fn same_owner_factory_wrapper_is_reported_but_other_owner_is_not() {
    let values = findings(
        r#"
struct Session;
impl Session { fn create() -> Self { Self } }
fn session() -> Session { Session::create() }
struct Plan;
struct Builder;
impl Builder { fn plan() -> Plan { Plan } }
fn plan() -> Plan { Builder::plan() }
"#,
    );
    assert_eq!(
        values
            .iter()
            .map(|finding| finding.subject.as_str())
            .collect::<Vec<_>>(),
        ["session"]
    );
}
