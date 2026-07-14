//! SANITY: base images carry NO compiler. Pure shell (no fork/exec of a toolchain binary) → should
//! pass on hl; not xfailed.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();

    v.push(
        scen("toolchains/ubuntu-no-cc", "ubuntu:24.04")
            .exec("command -v gcc || echo NO-CC")
            .has("NO-CC"),
    );
    v.push(
        scen("toolchains/debian-no-cc", "debian:bookworm")
            .exec("command -v cc || echo NO-CC")
            .has("NO-CC"),
    );
    v.push(
        scen("toolchains/alpine-no-cc", "alpine:latest")
            .exec("command -v gcc || echo NO-CC")
            .has("NO-CC"),
    );

    v
}
