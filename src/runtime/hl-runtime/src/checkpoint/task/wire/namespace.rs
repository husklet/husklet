// The wire modules mirror the whole task-registry vocabulary they serialize.
#![allow(clippy::wildcard_imports)]

use super::*;

impl NamespaceWire {
    pub(super) fn from_value(value: NamespaceId) -> Self {
        Self {
            kind: match value.kind {
                NamespaceKind::Uts => 1,
                NamespaceKind::Ipc => 2,
                NamespaceKind::Network => 3,
                NamespaceKind::Mount => 4,
                NamespaceKind::User => 5,
                NamespaceKind::Pid => 6,
            },
            serial: value.serial,
        }
    }
    pub(super) fn into_value(self) -> Result<NamespaceId, ()> {
        if self.serial == 0 {
            return Err(());
        }
        Ok(NamespaceId {
            kind: match self.kind {
                1 => NamespaceKind::Uts,
                2 => NamespaceKind::Ipc,
                3 => NamespaceKind::Network,
                4 => NamespaceKind::Mount,
                5 => NamespaceKind::User,
                6 => NamespaceKind::Pid,
                _ => return Err(()),
            },
            serial: self.serial,
        })
    }
}
impl NamespaceSetWire {
    pub(super) fn from_value(value: NamespaceSet) -> Self {
        Self {
            uts: NamespaceWire::from_value(value.uts),
            ipc: NamespaceWire::from_value(value.ipc),
            network: NamespaceWire::from_value(value.network),
            mount: NamespaceWire::from_value(value.mount),
            user: NamespaceWire::from_value(value.user),
            pid: NamespaceWire::from_value(value.pid),
        }
    }
    pub(super) fn into_value(self) -> Result<NamespaceSet, ()> {
        Ok(NamespaceSet {
            uts: NamespaceWire::into_value(self.uts)?,
            ipc: NamespaceWire::into_value(self.ipc)?,
            network: NamespaceWire::into_value(self.network)?,
            mount: NamespaceWire::into_value(self.mount)?,
            user: NamespaceWire::into_value(self.user)?,
            pid: NamespaceWire::into_value(self.pid)?,
        })
    }
}
impl UserNamespaceWire {
    pub(super) fn from_value(value: &UserNamespace) -> Self {
        Self {
            id: NamespaceWire::from_value(value.id),
            parent: value.parent.map(NamespaceWire::from_value),
            owner: value.owner,
        }
    }
    pub(super) fn into_value(self) -> Result<UserNamespace, ()> {
        Ok(UserNamespace {
            id: NamespaceWire::into_value(self.id)?,
            parent: self.parent.map(NamespaceWire::into_value).transpose()?,
            owner: self.owner,
        })
    }
}
