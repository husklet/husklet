//! Pull-backed web-server and proxy compatibility contracts loaded from a readable manifest.

use super::{contract::Group, runner::Runner};
use hl_container::Containers;

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    Runner::arm64(containers).run(group()).await
}

pub(crate) fn group() -> Group {
    Group::new(
        "web",
        crate::manifest::load(include_str!("../fixtures/web-core.yaml"))
            .expect("the checked-in web manifest must satisfy the schema"),
    )
}
