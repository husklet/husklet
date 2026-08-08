use hl_runtime::{AccessIdentity, Capabilities, RuntimePathError};
use hl_task::CapabilitySets;

use super::super::NativePath;

pub(in crate::ffi::linux::execution::path) struct Identity;

impl Identity {
    /// The filesystem capabilities the task actually holds. Every authorization site now projects
    /// `metadata::Registry` before comparing owners, so these no longer have to be pinned on to keep
    /// a dropped-privilege task from being denied its own files.
    fn capabilities(sets: CapabilitySets) -> Capabilities {
        let held = |capability: u64| sets.effective & capability != 0;
        Capabilities {
            dac_override: held(CapabilitySets::DAC_OVERRIDE),
            dac_read_search: held(CapabilitySets::DAC_READ_SEARCH),
            owner_override: held(CapabilitySets::OWNER_OVERRIDE),
            change_owner: held(CapabilitySets::CHANGE_OWNER),
            preserve_set_id: held(CapabilitySets::PRESERVE_SET_ID),
        }
    }

    pub(in crate::ffi::linux::execution::path) fn project(
        host: &NativePath,
    ) -> Result<AccessIdentity, RuntimePathError> {
        let tasks = host.tasks.as_ref().ok_or(RuntimePathError::Access)?;
        let process = host.process.ok_or(RuntimePathError::Access)?;
        let credentials = tasks.credentials(process).map_err(|_| RuntimePathError::Access)?;
        Ok(AccessIdentity {
            user: credentials.filesystem_user,
            group: credentials.filesystem_group,
            supplementary_groups: credentials.supplementary_groups().to_vec(),
            capabilities: Self::capabilities(credentials.capabilities),
        })
    }

    pub(in crate::ffi::linux::execution::path) fn access(
        host: &NativePath,
        effective: bool,
    ) -> Result<AccessIdentity, RuntimePathError> {
        let tasks = host.tasks.as_ref().ok_or(RuntimePathError::Access)?;
        let process = host.process.ok_or(RuntimePathError::Access)?;
        let credentials = tasks.credentials(process).map_err(|_| RuntimePathError::Access)?;
        Ok(AccessIdentity {
            user: if effective {
                credentials.effective_user
            } else {
                credentials.real_user
            },
            group: if effective {
                credentials.effective_group
            } else {
                credentials.real_group
            },
            supplementary_groups: credentials.supplementary_groups().to_vec(),
            capabilities: Self::capabilities(credentials.capabilities),
        })
    }
}
