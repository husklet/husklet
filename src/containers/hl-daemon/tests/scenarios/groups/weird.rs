//! Exact edge-runtime, native syscall, code-generation, and CPU contracts.
//!
//! The command/output matrix is declarative (`fixtures/weird-core.yaml`); only the
//! expected-failure accounting self-test stays in Rust.

use super::{contract, runner::Runner};
use hl_container::Containers;

type Error = Box<dyn std::error::Error>;

pub(crate) fn group() -> contract::Group {
    contract::Group::new(
        "weird",
        crate::manifest::load(include_str!("../fixtures/weird-core.yaml"))
            .expect("the checked-in weird manifest must satisfy the schema"),
    )
}

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    Runner::arm64(containers).run(group()).await
}

pub(crate) fn test_expected_failures() -> Result<(), String> {
    let expected = ["weird/dotnet-ryujit", "weird/io-uring"];
    for target in [contract::Target::Arm64, contract::Target::Amd64] {
        let mut actual = group()
            .scenarios
            .into_iter()
            .filter(|scenario| scenario.expected_failures.contains(&target))
            .map(|scenario| scenario.id)
            .collect::<Vec<_>>();
        actual.sort_unstable();
        if actual != expected {
            return Err(format!(
                "weird {target:?} expected-failure accounting drift: expected {expected:?}, got {actual:?}"
            ));
        }
    }
    Ok(())
}
