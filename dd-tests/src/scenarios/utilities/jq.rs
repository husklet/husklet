//! jq — VM over fixed JSON (entrypoint=jq → run-form).

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- jq (VM over fixed JSON; entrypoint=jq → run-form) -----------------------------------
        scen("utilities/jq-add", "ghcr.io/jqlang/jq:latest")
            .run(&["-n", "[range(1;1001)]|add"])
            .has("500500"),
        scen("utilities/jq-object", "ghcr.io/jqlang/jq:latest")
            .run(&["-n", "{a:1,b:2}|.a+.b"])
            .has("3"),
        scen("utilities/jq-sort", "ghcr.io/jqlang/jq:latest")
            .run(&["-nc", "[3,1,2]|sort"])
            .has("[1,2,3]"),
        scen("utilities/jq-version", "ghcr.io/jqlang/jq:latest")
            .run(&["--version"])
            .has("jq-1."),
    ]
}
