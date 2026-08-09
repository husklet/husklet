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

    /// `SECBIT_KEEP_CAPS` and `SECBIT_NO_SETUID_FIXUP` as `prctl(PR_SET_SECUREBITS)` encodes them.
    const SECURE_NO_SETUID_FIXUP: u32 = 1 << 2;
    const SECURE_KEEP_CAPS: u32 = 1 << 4;
    const SECURE_KEEP_CAPS_LOCKED: u32 = 1 << 5;
    /// `CAP_FS_MASK`: the chown, DAC, owner, set-id, immutable, mknod and MAC capabilities that
    /// `setfsuid` moves.
    const FILESYSTEM_MASK: u64 = 0b1_1111 | (1 << 9) | (1 << 27) | (1 << 32) | (1 << 33);

    #[must_use]
    pub fn supplementary_groups(&self) -> &[u32] {
        &self.groups
    }

    #[must_use]
    pub const fn has_capability(&self, capability: u64) -> bool {
        self.capabilities.effective & capability != 0
    }

    /// Linux `cap_task_prctl`: `PR_SET_KEEPCAPS` is refused in both directions, measured on the
    /// host, once `SECBIT_KEEP_CAPS_LOCKED` is set. Returns whether the request was honoured.
    pub fn set_keep_capabilities(&mut self, value: bool) -> bool {
        if self.secure_bits & Self::SECURE_KEEP_CAPS_LOCKED != 0 {
            return false;
        }
        self.keep_capabilities = value;
        true
    }

    /// Linux `cap_capset`: effective and permitted are bounded by the current permitted set, and a
    /// new inheritable capability must also lie in the bounding set. The bounding clause is
    /// unconditional in the kernel, so `PR_CAPBSET_DROP` of a still-permitted capability makes it
    /// bite even for a holder of `CAP_SETPCAP` (measured on the host).
    #[must_use]
    pub const fn may_replace_capabilities(&self, requested: CapabilitySets) -> bool {
        let current = self.capabilities;
        requested.effective & !requested.permitted == 0
            && requested.permitted & !current.permitted == 0
            && requested.inheritable & !(current.inheritable | current.permitted) == 0
            && requested.inheritable & !(current.inheritable | self.capability_bounding) == 0
            && (requested.effective | requested.permitted | requested.inheritable) & !CapabilitySets::SUPPORTED == 0
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

    /// Linux `cap_emulate_setxuid`: a uid transition drops capabilities and never gains permitted ones.
    /// Group transitions leave capabilities untouched, exactly as the kernel does.
    pub fn apply_setuid_capabilities(&mut self, old: [u32; 3]) {
        if self.secure_bits & Self::SECURE_NO_SETUID_FIXUP != 0 {
            return;
        }
        let [old_real, old_effective, old_saved] = old;
        let held_root = old_real == 0 || old_effective == 0 || old_saved == 0;
        let keeps_root = self.real_user == 0 || self.effective_user == 0 || self.saved_user == 0;
        if held_root && !keeps_root {
            if !self.keep_capabilities && self.secure_bits & Self::SECURE_KEEP_CAPS == 0 {
                self.capabilities.permitted = 0;
                self.capabilities.effective = 0;
            }
            self.capabilities.ambient = 0;
        }
        if old_effective == 0 && self.effective_user != 0 {
            self.capabilities.effective = 0;
        } else if old_effective != 0 && self.effective_user == 0 {
            self.capabilities.effective = self.capabilities.permitted;
        }
        hl_log::hl_debug!(
            hl_log::tag::TASK,
            "setuid capabilities uid={old_real}/{old_effective}/{old_saved} -> {}/{}/{} keep_caps={} permitted={:#x} effective={:#x} inheritable={:#x} ambient={:#x}",
            self.real_user,
            self.effective_user,
            self.saved_user,
            self.keep_capabilities,
            self.capabilities.permitted,
            self.capabilities.effective,
            self.capabilities.inheritable,
            self.capabilities.ambient
        );
    }

    /// Linux `LSM_SETID_FS`: only the filesystem capabilities move, and only within the permitted set.
    pub fn apply_setfsuid_capabilities(&mut self, old_filesystem_user: u32) {
        if self.secure_bits & Self::SECURE_NO_SETUID_FIXUP != 0 {
            return;
        }
        if old_filesystem_user == 0 && self.filesystem_user != 0 {
            self.capabilities.effective &= !Self::FILESYSTEM_MASK;
        } else if old_filesystem_user != 0 && self.filesystem_user == 0 {
            self.capabilities.effective |= self.capabilities.permitted & Self::FILESYSTEM_MASK;
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

    // The corpus case covers the reachable transitions; SECBIT_NO_SETUID_FIXUP and SECBIT_KEEP_CAPS
    // are only settable with CAP_SETPCAP under the securebits locking rules, so they are held here.
    #[test]
    fn securebits_suppress_the_setuid_fixup() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.secure_bits = 1 << 2;
        credentials.real_user = 501;
        credentials.effective_user = 501;
        credentials.saved_user = 501;
        credentials.apply_setuid_capabilities([0, 0, 0]);
        assert_eq!(credentials.capabilities.effective, CapabilitySets::CONTAINER);
        assert_eq!(credentials.capabilities.permitted, CapabilitySets::CONTAINER);
    }

    #[test]
    fn keep_caps_securebit_holds_permitted_but_not_effective() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.secure_bits = 1 << 4;
        credentials.real_user = 501;
        credentials.effective_user = 501;
        credentials.saved_user = 501;
        credentials.apply_setuid_capabilities([0, 0, 0]);
        assert_eq!(credentials.capabilities.permitted, CapabilitySets::CONTAINER);
        assert_eq!(credentials.capabilities.effective, 0);
    }

    #[test]
    fn keep_caps_lock_refuses_both_directions() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        assert!(credentials.set_keep_capabilities(true));
        credentials.secure_bits = 1 << 5;
        assert!(!credentials.set_keep_capabilities(false));
        assert!(credentials.keep_capabilities);
        assert!(!credentials.set_keep_capabilities(true));
        credentials.secure_bits = 0;
        assert!(credentials.set_keep_capabilities(false));
        assert!(!credentials.keep_capabilities);
    }

    #[test]
    fn capset_refuses_inheritable_outside_the_bounding_set() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        let net_raw = 1_u64 << 13;
        let mut requested = credentials.capabilities;
        requested.inheritable = net_raw;
        assert!(credentials.may_replace_capabilities(requested));
        // `PR_CAPBSET_DROP` leaves the capability permitted, which is how the two sets diverge.
        credentials.capability_bounding &= !net_raw;
        assert!(credentials.capabilities.permitted & net_raw != 0);
        assert!(!credentials.may_replace_capabilities(requested));
        requested.inheritable = 0;
        assert!(credentials.may_replace_capabilities(requested));
    }

    #[test]
    fn capset_refuses_permitted_growth() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.capabilities.permitted = 0;
        credentials.capabilities.effective = 0;
        let requested = CapabilitySets {
            effective: 0,
            permitted: CapabilitySets::SET_USER,
            inheritable: 0,
            ambient: 0,
        };
        assert!(!credentials.may_replace_capabilities(requested));
    }

    #[test]
    fn restore_rejects_impossible_authority() {
        let credentials = ProcessCredentials::new(501, 20, &[], 8).unwrap();
        assert_eq!(SetIdAuthority::try_from([false, true]), Err(TaskError::InvalidSnapshot));
        assert_eq!(credentials.setid_authority(), SetIdAuthority::None);
    }
}
