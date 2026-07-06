//! fork-heavy shell workflows — the core JIT stressor (fork/exec of expr / subshells).

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- fork-heavy shell workflows (the core JIT stressor) ----------------------------------
        // S1: ~2000 fork/exec of expr per iteration. musl applet re-exec.
        scen("utilities/fork-loop", "alpine")
            .exec("s=0; i=1; while [ $i -le 1000 ]; do s=$(expr $s + $i); i=$(expr $i + 1); done; echo \"SUM=$s\"")
            .has("SUM=500500"),
        // glibc variant — different fork/exec + dynamic-linker path.
        scen("utilities/fork-loop-glibc", "debian:bookworm")
            .exec("s=0; i=1; while [ $i -le 1000 ]; do s=$(expr $s + $i); i=$(expr $i + 1); done; echo \"SUM=$s\"")
            .has("SUM=500500"),
        // S5: 500 forked subshells each spawning sh+expr → maximal process-tree churn. T=250500.
        scen("utilities/fork-subshells", "alpine")
            .exec("t=0; n=1; while [ $n -le 500 ]; do v=$(sh -c \"echo $((n*2))\"); t=$(expr $t + $v); n=$(expr $n + 1); done; echo \"T=$t\"")
            .has("T=250500"),
        scen("utilities/fork-subshells-glibc", "debian:bookworm")
            .exec("t=0; n=1; while [ $n -le 500 ]; do v=$(sh -c \"echo $((n*2))\"); t=$(expr $t + $v); n=$(expr $n + 1); done; echo \"T=$t\"")
            .has("T=250500"),
    ]
}
