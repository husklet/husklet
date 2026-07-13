//! bash builtins — run-form (bash:5.2 has bash only under /usr/local/bin).

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- bash builtins (run-form: bash:5.2 has bash only under /usr/local/bin) ----------------
        scen("utilities/bash-base-convert", "bash:5.2")
            .run(&["bash", "-c", "echo $((2#1010))"])
            .has("10"),
        scen("utilities/bash-brace-arith", "bash:5.2")
            .run(&["bash", "-c", "for i in {1..1000}; do :; done; echo $((1000*1001/2))"])
            .has("500500"),
        scen("utilities/bash-arrays", "bash:5.2")
            .run(&["bash", "-c", "a=(1 2 3); echo ${#a[@]}"])
            .has("3"),
        scen("utilities/bash-param-expand", "bash:5.2")
            .run(&["bash", "-c", "echo ${x:-default}"])
            .has("default"),
        scen("utilities/bash-version", "bash:5.2")
            .run(&["bash", "--version"])
            .has("version 5.2"),
    ]
}
