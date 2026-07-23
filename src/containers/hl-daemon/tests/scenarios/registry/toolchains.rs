//! Exact compiler and toolchain compatibility contracts.

use crate::contract::Group;

pub fn group() -> Group {
    Group::new(
        "toolchains",
        crate::manifest::load(include_str!("../fixtures/toolchains-core.yaml"))
            .expect("the checked-in toolchains manifest must satisfy the schema"),
    )
}
