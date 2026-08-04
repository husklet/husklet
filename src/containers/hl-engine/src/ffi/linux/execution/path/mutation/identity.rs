use hl_runtime::{AccessIdentity, Capabilities, RuntimePathError};

use super::super::NativePath;

pub(in crate::ffi::linux::execution::path) struct Identity;

impl Identity {
    pub(in crate::ffi::linux::execution::path) fn project(
        host: &NativePath,
    ) -> Result<AccessIdentity, RuntimePathError> {
        let tasks = host.tasks.as_ref().ok_or(RuntimePathError::Access)?;
        let process = host.process.ok_or(RuntimePathError::Access)?;
        let credentials = tasks.credentials(process).map_err(|_| RuntimePathError::Access)?;
        let effective = credentials.capabilities.effective;
        Ok(AccessIdentity {
            user: credentials.filesystem_user,
            group: credentials.filesystem_group,
            supplementary_groups: credentials.supplementary_groups().to_vec(),
            capabilities: Capabilities {
                dac_override: effective & hl_task::CapabilitySets::DAC_OVERRIDE != 0,
                dac_read_search: effective & hl_task::CapabilitySets::DAC_READ_SEARCH != 0,
                owner_override: effective & hl_task::CapabilitySets::OWNER_OVERRIDE != 0,
                change_owner: effective & hl_task::CapabilitySets::CHANGE_OWNER != 0,
                preserve_set_id: effective & hl_task::CapabilitySets::PRESERVE_SET_ID != 0,
            },
        })
    }

    pub(in crate::ffi::linux::execution::path) fn access(
        host: &NativePath,
        effective: bool,
    ) -> Result<AccessIdentity, RuntimePathError> {
        let tasks = host.tasks.as_ref().ok_or(RuntimePathError::Access)?;
        let process = host.process.ok_or(RuntimePathError::Access)?;
        let credentials = tasks.credentials(process).map_err(|_| RuntimePathError::Access)?;
        let capability_set = if effective {
            credentials.capabilities.effective
        } else {
            credentials.capabilities.permitted
        };
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
            capabilities: Capabilities {
                dac_override: capability_set & hl_task::CapabilitySets::DAC_OVERRIDE != 0,
                dac_read_search: capability_set & hl_task::CapabilitySets::DAC_READ_SEARCH != 0,
                owner_override: capability_set & hl_task::CapabilitySets::OWNER_OVERRIDE != 0,
                change_owner: capability_set & hl_task::CapabilitySets::CHANGE_OWNER != 0,
                preserve_set_id: capability_set & hl_task::CapabilitySets::PRESERVE_SET_ID != 0,
            },
        })
    }
}
