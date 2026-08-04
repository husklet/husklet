//! Guest utility compatibility cases.

use crate::runner::Runner;
use hl_container::Containers;

type Error = Box<dyn std::error::Error>;

pub(crate) fn group() -> crate::contract::Group {
    crate::contract::Group::new(
        "utilities",
        crate::manifest::load(include_str!("../fixtures/utilities-core.yaml"))
            .expect("the checked-in utility manifest must satisfy the schema"),
    )
}

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    Runner::from_env(containers)?.run(group()).await
}
