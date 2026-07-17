//! The runtime workflows, one operation per file, run in the fixed order a decoded batch flows through:
//! [`negotiate`] (once, at connect) → [`validate`] (shape/limits, read-only) → [`account`] (transactional
//! residency charge) → [`dispatch`] (executor + timeline). Each takes a `&mut Session` (+ the injected
//! executor where needed); none holds state of its own.

pub mod account;
pub mod dispatch;
pub mod negotiate;
pub mod validate;
