// Category modules reference the testing engine via `super::` (they used to live at
// the crate root). Re-export the engine + registry here so those paths keep resolving.
#[allow(unused_imports, reason = "referenced by individual category modules via super::")]
pub(crate) use crate::{contract, fixture, registry, runner};

pub(crate) mod observe;
