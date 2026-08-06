use std::sync::atomic::{AtomicU16, Ordering};

use crate::{Kind, Metadata, Permissions};

/// Requested access bits corresponding to Linux `R_OK/W_OK/X_OK`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Access(u8);

impl Access {
    pub const EXECUTE: u8 = 1;
    pub const WRITE: u8 = 2;
    pub const READ: u8 = 4;

    pub fn from_bits(bits: u8) -> Result<Self, AccessError> {
        if bits & !0o7 != 0 {
            return Err(AccessError::InvalidAccess);
        }
        Ok(Self(bits))
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Capability subset consumed by filesystem permission policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    pub dac_override: bool,
    pub dac_read_search: bool,
    pub owner_override: bool,
    pub change_owner: bool,
    pub preserve_set_id: bool,
}

/// Filesystem-facing identity projection used for one permission decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessIdentity {
    pub user: u32,
    pub group: u32,
    pub supplementary_groups: Vec<u32>,
    pub capabilities: Capabilities,
}

impl AccessIdentity {
    #[must_use]
    pub fn belongs_to_group(&self, group: u32) -> bool {
        self.group == group || self.supplementary_groups.contains(&group)
    }

    pub fn check_access(&self, metadata: &Metadata, access: Access) -> Result<(), AccessError> {
        let requested = access.bits();
        if requested == 0 {
            return Ok(());
        }
        if self.capabilities.dac_override && Self::override_allows_execute(metadata, requested) {
            return Ok(());
        }
        let mut remaining = requested;
        if self.capabilities.dac_read_search {
            remaining &= !Access::READ;
            if metadata.kind == Kind::Directory {
                remaining &= !Access::EXECUTE;
            }
        }
        let granted = self.class_permissions(metadata);
        if remaining & !granted == 0 {
            Ok(())
        } else {
            Err(AccessError::PermissionDenied)
        }
    }

    pub fn chmod(&self, metadata: &Metadata, requested: Permissions) -> Result<Metadata, AccessError> {
        if self.user != metadata.user && !self.capabilities.owner_override {
            return Err(AccessError::OperationNotPermitted);
        }
        let mut permissions = requested;
        if permissions.bits() & Permissions::SET_GROUP_ID != 0
            && !self.belongs_to_group(metadata.group)
            && !self.capabilities.preserve_set_id
        {
            permissions = Permissions::from_bits(permissions.bits() & !Permissions::SET_GROUP_ID);
        }
        let mut changed = metadata.clone();
        changed.permissions = permissions;
        Ok(changed)
    }

    pub fn chown(&self, metadata: &Metadata, user: Option<u32>, group: Option<u32>) -> Result<Metadata, AccessError> {
        let requested_user = user.unwrap_or(metadata.user);
        let requested_group = group.unwrap_or(metadata.group);
        let privileged = self.capabilities.change_owner;
        let owner_group_change =
            self.user == metadata.user && requested_user == metadata.user && self.belongs_to_group(requested_group);
        if !privileged && !owner_group_change {
            return Err(AccessError::OperationNotPermitted);
        }
        let mut changed = metadata.clone();
        changed.user = requested_user;
        changed.group = requested_group;
        if user.is_some() || group.is_some() {
            changed.permissions = Permissions::from_bits(
                changed.permissions.bits() & !(Permissions::SET_USER_ID | Permissions::SET_GROUP_ID),
            );
        }
        Ok(changed)
    }

    fn class_permissions(&self, metadata: &Metadata) -> u8 {
        let shift = if self.user == metadata.user {
            6
        } else if self.belongs_to_group(metadata.group) {
            3
        } else {
            0
        };
        metadata.permissions.class_bits(shift) as u8
    }

    fn override_allows_execute(metadata: &Metadata, requested: u8) -> bool {
        requested & Access::EXECUTE == 0 || metadata.kind == Kind::Directory || metadata.permissions.any_execute()
    }
}

/// Stable permission-policy failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessError {
    InvalidAccess,
    PermissionDenied,
    OperationNotPermitted,
}

/// Process-local creation mask with atomic replace-and-return semantics.
#[derive(Debug)]
pub struct Umask {
    bits: AtomicU16,
}

impl Umask {
    #[must_use]
    pub const fn new(bits: u16) -> Self {
        Self {
            bits: AtomicU16::new(bits & 0o777),
        }
    }

    #[must_use]
    pub fn replace(&self, bits: u16) -> Permissions {
        Permissions::from_bits(self.bits.swap(bits & 0o777, Ordering::AcqRel))
    }

    #[must_use]
    pub fn apply(&self, requested: Permissions) -> Permissions {
        let mask = self.bits.load(Ordering::Acquire);
        Permissions::from_bits(requested.bits() & !mask)
    }
}

impl Default for Umask {
    fn default() -> Self {
        Self::new(0o022)
    }
}
