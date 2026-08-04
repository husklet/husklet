use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, ProcessAbi};
use hl_task::{ProcessCredentials, ProcessSnapshot};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn snapshot(&self) -> Result<ProcessSnapshot, Errno> {
        self.tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|process| process.id == self.process)
            .ok_or(Errno::ESRCH)
    }

    pub(crate) fn getppid(&self) -> LinuxResult {
        match self.snapshot() {
            Ok(snapshot) => LinuxResult::Value(snapshot.parent.map_or(0, |parent| parent.number()) as u64),
            Err(error) => LinuxResult::Error(error),
        }
    }

    pub(crate) fn identity_value(&self, field: usize) -> LinuxResult {
        let credentials = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let value = match field {
            0 => credentials.real_user,
            1 => credentials.effective_user,
            2 => credentials.real_group,
            _ => credentials.effective_group,
        };
        LinuxResult::Value(value as u64)
    }

    pub(crate) fn getres(&self, arguments: [u64; 6], user: bool) -> LinuxResult {
        let credentials = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let values = if user {
            [
                credentials.real_user,
                credentials.effective_user,
                credentials.saved_user,
            ]
        } else {
            [
                credentials.real_group,
                credentials.effective_group,
                credentials.saved_group,
            ]
        };
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let staged = arguments[..3]
            .iter()
            .zip(values)
            .map(|(address, value)| abi.stage_id(*address, value))
            .collect::<Result<Vec<_>, _>>();
        let staged = match staged {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        for copyout in staged {
            if let Err(error) = copyout.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        LinuxResult::Value(0)
    }

    pub(crate) fn getgroups(&self, count: usize, address: u64) -> LinuxResult {
        if count > i32::MAX as usize {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let groups = match self.snapshot() {
            Ok(value) => value.credentials.supplementary_groups().to_vec(),
            Err(error) => return LinuxResult::Error(error),
        };
        if count == 0 {
            return LinuxResult::Value(groups.len() as u64);
        }
        if count < groups.len() {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let staged = match abi.stage_groups(address, &groups) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(groups.len() as u64),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn setgroups(&self, count: usize, address: u64) -> LinuxResult {
        if count > 65_536 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let current = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        if !current.may_setid() {
            return LinuxResult::Error(Errno::EPERM);
        }
        let groups = match ProcessAbi::new(&self.memory, self.architecture).groups(address, count) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.replace_groups(current, &groups)
    }

    pub(crate) fn set_single_id(&self, identifier: u32, user: bool) -> LinuxResult {
        let current = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let privileged = current.may_setid();
        let allowed = if user {
            [current.real_user, current.effective_user, current.saved_user]
        } else {
            [current.real_group, current.effective_group, current.saved_group]
        };
        if !privileged && !allowed.contains(&identifier) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let ids = if privileged {
            [identifier, identifier, identifier]
        } else {
            [u32::MAX, identifier, u32::MAX]
        };
        self.replace_ids(current, ids, user, true)
    }

    pub(crate) fn set_pair(&self, ids: [u32; 2], user: bool) -> LinuxResult {
        let current = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let privileged = current.may_setid();
        let held = if user {
            [current.real_user, current.effective_user, current.saved_user]
        } else {
            [current.real_group, current.effective_group, current.saved_group]
        };
        if !privileged
            && (ids[0] != u32::MAX && !held[..2].contains(&ids[0]) || ids[1] != u32::MAX && !held.contains(&ids[1]))
        {
            return LinuxResult::Error(Errno::EPERM);
        }
        let old_real = held[0];
        let effective = if ids[1] == u32::MAX { held[1] } else { ids[1] };
        let saved = if ids[0] != u32::MAX || ids[1] != u32::MAX && ids[1] != old_real {
            effective
        } else {
            u32::MAX
        };
        self.replace_ids(current, [ids[0], ids[1], saved], user, true)
    }

    pub(crate) fn set_ids(&self, ids: [u32; 3], user: bool) -> LinuxResult {
        let current = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let allowed = if user {
            [current.real_user, current.effective_user, current.saved_user]
        } else {
            [current.real_group, current.effective_group, current.saved_group]
        };
        if !current.may_setid() && !Self::ids_permitted(ids, allowed) {
            return LinuxResult::Error(Errno::EPERM);
        }
        self.replace_ids(current, ids, user, true)
    }

    pub(crate) fn set_filesystem(&self, identifier: u32, user: bool) -> LinuxResult {
        let mut current = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        let (old, allowed) = if user {
            (
                current.filesystem_user,
                [current.real_user, current.effective_user, current.saved_user],
            )
        } else {
            (
                current.filesystem_group,
                [current.real_group, current.effective_group, current.saved_group],
            )
        };
        if identifier != u32::MAX && (current.may_setid() || allowed.contains(&identifier) || old == identifier) {
            if user {
                current.filesystem_user = identifier;
            } else {
                current.filesystem_group = identifier;
            }
            if let LinuxResult::Error(error) = self.replace_credentials(current) {
                return LinuxResult::Error(error);
            }
        }
        LinuxResult::Value(old as u64)
    }

    fn ids_permitted(ids: [u32; 3], allowed: [u32; 3]) -> bool {
        ids.into_iter().all(|id| id == u32::MAX || allowed.contains(&id))
    }

    fn replace_ids(
        &self,
        mut credentials: ProcessCredentials,
        ids: [u32; 3],
        user: bool,
        sync_filesystem: bool,
    ) -> LinuxResult {
        let target = if user {
            [
                &mut credentials.real_user,
                &mut credentials.effective_user,
                &mut credentials.saved_user,
            ]
        } else {
            [
                &mut credentials.real_group,
                &mut credentials.effective_group,
                &mut credentials.saved_group,
            ]
        };
        for (field, value) in target.into_iter().zip(ids) {
            if value != u32::MAX {
                *field = value;
            }
        }
        if !sync_filesystem {
            return self.replace_credentials(credentials);
        }
        if !user {
            credentials.filesystem_group = credentials.effective_group;
            return self.replace_credentials(credentials);
        }
        credentials.filesystem_user = credentials.effective_user;
        credentials.refresh_setid();
        self.replace_credentials(credentials)
    }

    fn replace_groups(&self, current: ProcessCredentials, groups: &[u32]) -> LinuxResult {
        let mut replacement = current;
        if replacement.replace_groups(groups, 65_536).is_err() {
            return LinuxResult::Error(Errno::EINVAL);
        }
        self.replace_credentials(replacement)
    }

    pub(crate) fn replace_credentials(&self, credentials: ProcessCredentials) -> LinuxResult {
        match self.tasks.replace_credentials(self.process, credentials) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EINVAL),
        }
    }

    pub(crate) fn umask(&self, mask: u32) -> LinuxResult {
        LinuxResult::Value(self.fs_context.replace_mask(mask) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_task::CapabilitySets;

    #[test]
    fn rootless_mask_does_not_grant_setid() {
        let mut credentials = ProcessCredentials::new(501, 20, &[], 8).unwrap();
        credentials.capabilities.effective = CapabilitySets::CONTAINER;
        credentials.capabilities.permitted = CapabilitySets::CONTAINER;
        credentials.keep_capabilities = true;
        credentials.refresh_setid();
        assert_eq!(credentials.capabilities.effective, CapabilitySets::CONTAINER);
        assert_eq!(credentials.capabilities.permitted, CapabilitySets::CONTAINER);
        assert!(!credentials.may_setid());

        credentials.restore_setid_authority(hl_task::SetIdAuthority::Permitted);
        credentials.raise_setid();
        assert!(credentials.may_setid());
    }
}
