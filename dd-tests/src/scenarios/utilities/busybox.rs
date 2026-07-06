//! busybox — single static musl multi-call binary applets.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- busybox (single static musl multi-call binary) --------------------------------------
        scen("utilities/busybox-arith", "busybox:latest")
            .run(&["sh", "-c", "echo $((7*6))"])
            .has("42"),
        scen("utilities/busybox-pipe", "busybox:latest")
            .run(&["sh", "-c", "seq 1 1000 | awk '{s+=$1}END{print s}'"])
            .has("500500"),
        scen("utilities/busybox-banner", "busybox:latest")
            .exec("busybox 2>&1 | head -1")
            .has("BusyBox v1.3"),
        scen("utilities/busybox-fork-loop", "busybox:latest")
            .exec("s=0; i=1; while [ $i -le 1000 ]; do s=$(expr $s + $i); i=$(expr $i + 1); done; echo \"SUM=$s\"")
            .has("SUM=500500"),
        scen("utilities/busybox-sha256", "busybox:latest")
            .exec("printf abc | sha256sum | cut -d' ' -f1")
            .has("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
    ]
}
