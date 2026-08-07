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
            user_map: value.user_map.as_ref().map(Self::map_wire),
            group_map: value.group_map.as_ref().map(Self::map_wire),
            setgroups: match value.setgroups {
                SetgroupsState::Allow => 1,
                SetgroupsState::Deny => 2,
            },
            user_authority: value.user_authority,
            group_authority: value.group_authority,
        }
    }
    pub(super) fn into_value(self) -> Result<UserNamespace, ()> {
        Ok(UserNamespace {
            id: NamespaceWire::into_value(self.id)?,
            parent: self.parent.map(NamespaceWire::into_value).transpose()?,
            owner: self.owner,
            user_map: self.user_map.map(Self::map_value).transpose()?,
            group_map: self.group_map.map(Self::map_value).transpose()?,
            setgroups: match self.setgroups {
                1 => SetgroupsState::Allow,
                2 => SetgroupsState::Deny,
                _ => return Err(()),
            },
            user_authority: self.user_authority,
            group_authority: self.group_authority,
        })
    }
    pub(super) fn map_wire(value: &IdMap) -> Vec<[u32; 3]> {
        value
            .ranges()
            .iter()
            .map(|range| [range.inside, range.outside, range.length])
            .collect()
    }
    pub(super) fn map_value(value: Vec<[u32; 3]>) -> Result<IdMap, ()> {
        IdMap::new(
            &value
                .into_iter()
                .map(|item| IdRange {
                    inside: item[0],
                    outside: item[1],
                    length: item[2],
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|_| ())
    }
}
