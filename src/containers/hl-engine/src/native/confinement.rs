//! Linux deny-default worker confinement capability.

use crate::engine::EngineError;

pub struct HostConfinement;

impl HostConfinement {
    pub fn apply() -> Result<(), EngineError> {
        crate::ffi::linux::Seccomp::apply().map_err(|_| EngineError::AuthorityFailed)
    }

    #[must_use]
    pub fn variants_denied() -> bool {
        crate::ffi::linux::Seccomp::variants_denied()
    }
}
