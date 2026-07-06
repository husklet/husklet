//! tiny static exec path — hello-world (known dd loader gap → xfail on both linux arches).

use crate::scenario::{scen, Scenario, Target};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- tiny static exec path ---------------------------------------------------------------
        // passes on Real; known dd loader gap (exec-loader-noent) on both linux arches → xfail.
        scen("utilities/hello-world", "hello-world:latest")
            .run(&[])
            .has("Hello from Docker!")
            .xfail(&[Target::ArmLinux, Target::AmdLinux]),
    ]
}
