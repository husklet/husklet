use hl_runtime::{AccessIdentity, Capabilities, RuntimePathError};

use super::super::NativePath;

pub(in crate::ffi::linux::execution::path) struct Identity;

impl Identity {
    /// The filesystem checks compare a guest id against the HOST inode owner: `Descriptor::metadata`
    /// fstats the host file, and only chown records a guest owner, so almost every path reports the
    /// engine's own uid. Until created files carry a guest owner, an honest projection would deny a
    /// dropped-privilege task every mutation outside a world-writable directory -- measured EACCES on
    /// both a root-chowned directory and one the task created itself. The DAC bypasses therefore stay
    /// pinned on here, so this layer is exactly as permissive as before capabilities became droppable;
    /// making ownership real is what unpins them.
    const BYPASS: Capabilities = Capabilities {
        dac_override: true,
        dac_read_search: true,
        owner_override: true,
        change_owner: true,
        preserve_set_id: true,
    };

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
            capabilities: Self::BYPASS,
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
            capabilities: Self::BYPASS,
        })
    }
}
