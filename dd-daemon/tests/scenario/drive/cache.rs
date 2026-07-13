//! Run-wide memoization for the scenario runner. The runner used to pay a `mac`-bridge round-trip for
//! EVERY scenario×target just to re-confirm the image is present, and re-ran the (deterministic) real-
//! docker oracle every time. Both verdicts are invariant for a whole run, so we memoize them here.
//! Combined with the worker pool in `scenarios.rs` this turns the lane from one serial bridge call per
//! cell into a handful of cached, parallel ones.
use super::*;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// `image → availability` verdict, computed once per image instead of once per scenario×target.
pub(super) fn ensure_cache() -> &'static Mutex<HashMap<String, bool>> {
    static C: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Real-backend (oracle) output cache, keyed by the logical run `(image,step,target,tty)`. The oracle
/// is ground-truth-deterministic, so an identical cell never needs to hit the bridge twice.
pub(super) fn oracle_cache() -> &'static Mutex<HashMap<String, (String, i32)>> {
    static C: OnceLock<Mutex<HashMap<String, (String, i32)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The logical key that fully determines a cell's output (no pid/host noise → stable across cells).
pub(super) fn cell_key(s: &Scenario, t: Target) -> String {
    let step = match &s.step {
        Step::Run(a) => format!("run\u{1}{}", a.join("\u{1}")),
        Step::ExecIt(x) => format!("exec\u{1}{x}"),
        Step::Host(x) => format!("host\u{1}{x}"),
    };
    format!("{}\u{1}{}\u{1}{}\u{1}{}", s.image, step, t.label(), s.tty)
}
