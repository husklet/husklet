//! tiny static exec path — hello-world (known hl loader gap → xfail on both linux arches).

use crate::scenario::{scen, Scenario, Target};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- tiny static exec path ---------------------------------------------------------------
        // passes on Real; known hl loader gap (exec-loader-noent) on both linux arches → xfail.
        scen("utilities/hello-world", "hello-world:latest")
            .run(&[])
            .has("Hello from Docker!")
            .xfail(&[Target::ArmLinux, Target::AmdLinux]),
    ]
}
