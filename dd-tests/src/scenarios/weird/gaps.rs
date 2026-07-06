//! Seed: documented dd divergences (GAPS.md) that are NOT toolchain-related — the CPU-topology
//! syscall gap (mongod aborts on an empty possible-CPU set) and the static non-PIE ET_EXEC loader
//! gap (hello-world). Both xfail on both Linux arches.

use crate::scenario::{scen, Scenario, Target};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // mongod aborts on empty possible-CPU set (tcmalloc) — a CPU-topology syscall gap. xfail+GAPS.
        scen("weird/mongo-cpu-topology", "mongo")
            .run(&["mongod", "--version"]).has("db version")
            .xfail(&[Target::ArmLinux, Target::AmdLinux]),   // NumPossibleCPUs empty — GAPS.md
        // hello-world: the canonical static non-PIE ET_EXEC binary — the exec/loader gap's repro. xfail.
        scen("weird/static-nonpie-helloworld", "hello-world")
            .run(&[]).has("Hello from Docker")
            .xfail(&[Target::ArmLinux, Target::AmdLinux]),   // non-PIE exec loader gap — GAPS.md
    ]
}
