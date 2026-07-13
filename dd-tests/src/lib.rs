//! dd-tests — a declarative test harness that runs guest programs across every JIT engine.
//!
//! A [`Case`] is one guest program + its expected behaviour. Cases are organised into named
//! [`Group`]s. The runner executes the **engine × case** matrix: each case runs on every engine whose
//! guest architecture it can be provisioned for (aarch64 guests are compiled on the fly; x86-64 guests
//! come from prebuilt fixtures, since there's no local cross-compiler). Checks are golden
//! (exit/stdout) or differential against a native oracle.
//!
//! ```ignore
//! group("compat", [
//!     src("hello", "hello.c").exit(42).out("hi\n"),
//!     src("sort",  "sort.c").oracle(),                 // diff vs native run
//! ])
//! ```

pub mod bench_gates; // guard helpers for `bin/bench` (BENCH_N / dd-lane / artifact-write gates)
pub mod cases;
pub mod diag; // turn a failed run into an actionable JIT bug report (signals/buckets/crash details); still consumed by the daemon scenario runner (now in dd-daemon) via the dd-tests dev-dep

// The real-software scenario surface (`scenario`/`scenarios` + the `scenarios` bin) moved to its owner
// `dd-daemon` (ownership-matrix Step 3): `cargo test -p dd-daemon --test scenarios`.

mod harness;
pub use harness::*;
