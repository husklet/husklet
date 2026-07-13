//! `cargo test -p dd-jit-darwin --test suite` runs the whole engine × case matrix; any failed case
//! fails the test. For granular grouped output + filtering use the runner: `cargo run -p dd-jit-darwin
//! --example matrix`.
//!
//! This is the CI matrix gate, so it must apply the same false-green guards the runner does (see
//! [`dd_tests::gate_failures`]): XPASS fails, a dark/unbuilt engine lane fails, and a matrix that ran
//! nothing fails. Previously it only asserted `failures.is_empty()` over Fail cells, so a build that
//! dropped an engine — or a fully-skipped matrix — reported "0 failed" while testing nothing.
//!
//! The case registry moved here with the engine (ownership-matrix Step 2); the harness model
//! (`Ctx`/`Engine`/`run`/`gate_failures`) is the product-neutral `dd_tests` dev-dependency.
use dd_tests::{gate_failures, run, Cell, Ctx, Engine};

#[path = "engine_matrix/mod.rs"]
mod cases;

#[test]
fn matrix() {
    let ctx = Ctx::discover();
    let mut cells = Vec::<Cell>::new();
    for g in cases::all() {
        for c in &g.cases {
            for e in Engine::ALL {
                cells.push(Cell {
                    name: format!("{}/{}", g.name, c.name),
                    engine: e,
                    status: run(&ctx, c, e),
                });
            }
        }
    }
    // The cargo-test path is always the full (unfiltered) run, so every engine is required.
    let failures = gate_failures(&Engine::ALL, &|e| e.available(), &cells);
    assert!(
        failures.is_empty(),
        "{} matrix cell(s) evaluated; {} gate failure(s):\n{}",
        cells.len(),
        failures.len(),
        failures.join("\n")
    );
}
