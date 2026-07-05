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

pub mod cases;
pub mod diag;
pub mod scenario; // real-software surface: drive popular images through dd-daemon (Real-oracle vs Dd)
pub mod scenarios; // the scenario registry (one folder per category) // turn a failed run into an actionable JIT bug report (signals/buckets/crash details)

mod harness;
pub use harness::*;
