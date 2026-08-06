//! Process credential identity, capability sets, and set-id authority.
use crate::TaskError;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessCredentials {
    pub real_user: u32,
    pub effective_user: u32,
    pub saved_user: u32,
    pub filesystem_user: u32,
    pub real_group: u32,
    pub effective_group: u32,
    pub saved_group: u32,
    pub filesystem_group: u32,
    pub capabilities: CapabilitySets,
    pub capability_bounding: u64,
    pub secure_bits: u32,
    pub keep_capabilities: bool,
    pub no_new_privileges: bool,
    /// Set-id authority is distinct from the guest-visible capability persona.
    setid_permitted: bool,
    /// Effective SETUID/SETGID authority, re-raised only from `setid_permitted`.
    setid_effective: bool,
    groups: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilitySets {
    pub effective: u64,
    pub permitted: u64,
    pub inheritable: u64,
    pub ambient: u64,
}

/// Set-id authority retained across credential transitions and checkpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetIdAuthority {
    None,
    Permitted,
    Effective,
}

impl TryFrom<[bool; 2]> for SetIdAuthority {
    type Error = TaskError;

    fn try_from([permitted, effective]: [bool; 2]) -> Result<Self, Self::Error> {
        match (permitted, effective) {
            (false, false) => Ok(Self::None),
            (true, false) => Ok(Self::Permitted),
            (true, true) => Ok(Self::Effective),
            (false, true) => Err(TaskError::InvalidSnapshot),
        }
    }
}

impl From<SetIdAuthority> for [bool; 2] {
    fn from(authority: SetIdAuthority) -> Self {
        match authority {
            SetIdAuthority::None => [false, false],
            SetIdAuthority::Permitted => [true, false],
            SetIdAuthority::Effective => [true, true],
        }
    }
}

impl CapabilitySets {
    pub const SUPPORTED: u64 = (1_u64 << 41) - 1;
    pub const CONTAINER: u64 = 0x0000_0000_a804_25fb;
    pub const KILL: u64 = 1 << 5;
    pub const CHANGE_OWNER: u64 = 1;
    pub const DAC_OVERRIDE: u64 = 1 << 1;
    pub const DAC_READ_SEARCH: u64 = 1 << 2;
    pub const OWNER_OVERRIDE: u64 = 1 << 3;
    pub const PRESERVE_SET_ID: u64 = 1 << 4;
    pub const SET_GROUP: u64 = 1 << 6;
    pub const SET_USER: u64 = 1 << 7;
    pub const SYS_ADMIN: u64 = 1 << 21;

    #[must_use]
    pub const fn initial(user: u32) -> Self {
        let permitted = if user == 0 { Self::CONTAINER } else { 0 };
        Self {
            effective: permitted,
            permitted,
            inheritable: 0,
            ambient: 0,
        }
    }
}

impl ProcessCredentials {
    pub fn new(user: u32, group: u32, groups: &[u32], max_groups: usize) -> Result<Self, TaskError> {
        if groups.len() > max_groups {
            return Err(TaskError::GroupLimit);
        }
        Ok(Self {
            real_user: user,
            effective_user: user,
            saved_user: user,
            filesystem_user: user,
            real_group: group,
            effective_group: group,
            saved_group: group,
            filesystem_group: group,
            capabilities: CapabilitySets::initial(user),
            capability_bounding: if user == 0 { CapabilitySets::CONTAINER } else { 0 },
            secure_bits: 0,
            keep_capabilities: false,
            no_new_privileges: false,
            setid_permitted: user == 0,
            setid_effective: user == 0,
            groups: groups.to_vec(),
        })
    }

    #[must_use]
    pub fn supplementary_groups(&self) -> &[u32] {
        &self.groups
    }

    #[must_use]
    pub const fn has_capability(&self, capability: u64) -> bool {
        self.capabilities.effective & capability != 0
    }

    #[must_use]
    pub const fn may_setid(&self) -> bool {
        self.setid_effective
    }

    /// Returns the permitted and effective set-id authority for checkpointing.
    #[must_use]
    pub const fn setid_authority(&self) -> SetIdAuthority {
        match (self.setid_permitted, self.setid_effective) {
            (false, false) => SetIdAuthority::None,
            (true, false) => SetIdAuthority::Permitted,
            (true, true) => SetIdAuthority::Effective,
            (false, true) => unreachable!(),
        }
    }

    /// Restores checkpointed set-id authority.
    pub fn restore_setid_authority(&mut self, authority: SetIdAuthority) {
        [self.setid_permitted, self.setid_effective] = authority.into();
    }

    pub fn refresh_setid(&mut self) {
        if self.effective_user == 0 {
            self.setid_permitted = true;
            self.setid_effective = true;
            return;
        }
        self.setid_effective = false;
        if self.real_user != 0 && self.saved_user != 0 && !self.keep_capabilities {
            self.setid_permitted = false;
        }
    }

    pub fn raise_setid(&mut self) {
        self.setid_effective = self.setid_permitted;
    }

    pub fn reset_setid_for_exec(&mut self) {
        self.keep_capabilities = false;
        self.setid_permitted = self.effective_user == 0;
        self.setid_effective = self.setid_permitted;
    }

    pub fn replace_groups(&mut self, groups: &[u32], max_groups: usize) -> Result<(), TaskError> {
        if groups.len() > max_groups {
            return Err(TaskError::GroupLimit);
        }
        self.groups = groups.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod credential_test {
    use super::*;

    #[test]
    fn nonroot_cannot_bootstrap_setid() {
        let mut credentials = ProcessCredentials::new(501, 20, &[], 8).unwrap();
        credentials.capabilities.effective = CapabilitySets::CONTAINER;
        credentials.capabilities.permitted = CapabilitySets::CONTAINER;
        credentials.keep_capabilities = true;
        credentials.raise_setid();
        assert!(!credentials.may_setid());
    }

    #[test]
    fn keepcaps_preserves_setid_permission() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.keep_capabilities = true;
        credentials.real_user = 501;
        credentials.effective_user = 501;
        credentials.saved_user = 501;
        credentials.refresh_setid();
        assert_eq!(credentials.setid_authority(), SetIdAuthority::Permitted);
        credentials.raise_setid();
        assert!(credentials.may_setid());
    }

    #[test]
    fn ordinary_drop_discards_setid() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.real_user = 501;
        credentials.effective_user = 501;
        credentials.saved_user = 501;
        credentials.refresh_setid();
        assert_eq!(credentials.setid_authority(), SetIdAuthority::None);
        credentials.raise_setid();
        assert!(!credentials.may_setid());
    }

    #[test]
    fn restore_rejects_impossible_authority() {
        let credentials = ProcessCredentials::new(501, 20, &[], 8).unwrap();
        assert_eq!(SetIdAuthority::try_from([false, true]), Err(TaskError::InvalidSnapshot));
        assert_eq!(credentials.setid_authority(), SetIdAuthority::None);
    }
}
