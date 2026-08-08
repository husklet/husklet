use hl_runtime::{AccessIdentity, Capabilities, RuntimePathError};

use super::super::NativePath;

pub(in crate::ffi::linux::execution::path) struct Identity;

impl Identity {
    /// Created files now carry a guest owner, so `metadata::Registry` can answer who owns an inode.
    /// These bypasses stay pinned on because two checks still read `attribute::Descriptor::metadata`,
    /// a raw fstat reporting the engine's host uid: `authorize_chmod`/`authorize_chown`/
    /// `authorize_times` here, and `PinnedEntry::parent_access`. Unpinning before those project the
    /// registry would deny a dropped-privilege task both metadata changes on files it owns and
    /// creation inside directories it made. Projecting that one `metadata()` is what unpins them.
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
