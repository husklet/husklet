//! Real-software scenario harness — the SECOND test surface, in Rust (no bash).
//!
//! Where `cases` runs compiled guests in-process through the JIT, `scenario` drives **real, popular
//! software** (postgres, redis, node, gcc, distros, …) through a container engine exactly as a developer
//! would. The container daemon is the *vehicle*; the dd **JIT engine is what's under test**.
//!
//! TWO BACKENDS (the key to fast, unblocked authoring):
//!   * [`Backend::Real`] — the host's real `docker`. The **oracle / ground truth**: every scenario must
//!     pass here, which proves the *test* is correct (deterministic, right marker). Authors verify here.
//!   * [`Backend::Dd`]   — `dd-daemon` (the system under test), driven via the `mac` bridge on a linux
//!     dev host (the daemon is a Mach-O binary; env is inline, socket/state under a `/Users` shared
//!     path) or directly on a real macOS host. Divergences from the oracle are dd bugs → `xfail` + GAPS.
//!
//! A [`Scenario`] is one image + how to drive it + what to expect, on each [`Target`] (linux/arm64,
//! linux/amd64; mac lighter-touch). See docs/CHARTER.md and docs/TESTING.md.
//!
//! ```ignore
//! scen("databases/redis-ping", "redis:alpine")
//!     .exec("redis-server --save '' --daemonize yes; sleep 1; redis-cli ping")  // exec -i /bin/sh path
//!     .has("PONG")
//!     .xfail(&[Target::ArmLinux])    // known dd fork+exec gap — passes on Real, xfail on Dd (GAPS.md)
//! ```

mod arch;
mod daemon;
mod drive;
mod model;

pub use arch::*;
pub use daemon::*;
pub use drive::*;
pub use model::*;
