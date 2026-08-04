// Category modules reference the testing engine via `super::` (they used to live at
// the crate root). Re-export the engine + registry here so those paths keep resolving.
#[allow(unused_imports, reason = "referenced by individual category modules via super::")]
pub(crate) use crate::{contract, fixture, registry, runner};

pub(crate) mod copy;
pub(crate) mod execcmd;
pub(crate) mod imagescmd;
pub(crate) mod languages;
pub(crate) mod netcontainer;
pub(crate) mod network;
pub(crate) mod observe;
pub(crate) mod runflags;
pub(crate) mod utilities;
pub(crate) mod volume;
pub(crate) mod weird;
