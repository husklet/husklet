//! Typed legacy network package-install contracts.

use crate::contract::Group;

pub fn group() -> Group {
    Group::new(
        "netinstall",
        crate::manifest::load(include_str!("../fixtures/netinstall-core.yaml"))
            .expect("the checked-in netinstall manifest must satisfy the schema"),
    )
}
