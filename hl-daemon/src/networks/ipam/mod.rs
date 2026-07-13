// ---- networks --------------------------------------------------------------
// IPAM split into static predefined-network data (`defaults`) and the address/subnet allocation logic
// (`alloc`); every item is glob-re-exported so `crate::networks::ipam::X` resolves unchanged.

mod alloc;
mod defaults;

pub(crate) use alloc::*;
pub(crate) use defaults::*;
