//! arithmetic pipelines — seq/awk/paste/bc multi-process sums.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- arithmetic pipelines ----------------------------------------------------------------
        // S2: two-process pipe, awk sum 1..1000 = 500500.
        scen("utilities/seq-awk-sum", "alpine")
            .exec("seq 1 1000 | awk '{s+=$1} END{print \"SUM=\"s}'")
            .has("SUM=500500"),
        // S3: 3-stage pipe through bc (arbitrary precision).
        scen("utilities/paste-bc", "alpine")
            .exec("seq 1 1000 | paste -sd+ - | bc")
            .has("500500"),
        scen("utilities/paste-bc-bash", "bash:5.2")
            .run(&["bash", "-c", "seq 1 1000 | paste -sd+ - | bc"])
            .has("500500"),
    ]
}
