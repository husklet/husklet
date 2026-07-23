//! Typed legacy single-container networking contracts.

use crate::contract::Group;

pub fn group() -> Group {
    Group::new(
        "networking",
        crate::manifest::load(include_str!("../fixtures/networking-core.yaml"))
            .expect("the checked-in networking manifest must satisfy the schema"),
    )
}
