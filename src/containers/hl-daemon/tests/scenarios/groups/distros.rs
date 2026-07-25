//! Distribution compatibility contracts loaded from a readable manifest.

use super::{contract::Group, runner::Runner};
use hl_container::Containers;

type Error = Box<dyn std::error::Error>;

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    Runner::from_env(containers)?.run(group()).await
}

pub(crate) fn group() -> Group {
    Group::new(
        "distros",
        crate::manifest::load(include_str!("../fixtures/distros-full.yaml"))
            .expect("the checked-in distro manifest must satisfy the schema"),
    )
}
