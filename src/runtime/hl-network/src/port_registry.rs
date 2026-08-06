//! Ephemeral and explicit port ownership keyed by address family.
use crate::{AddressFamily, SocketError, SocketId};
use std::collections::BTreeMap;
use std::sync::RwLock;
pub struct PortRegistry {
    owners: RwLock<BTreeMap<(AddressFamilyKey, u16), SocketId>>,
    first_ephemeral: u16,
    last_ephemeral: u16,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AddressFamilyKey {
    Inet4,
    Inet6,
}

impl PortRegistry {
    #[must_use]
    pub const fn new(first_ephemeral: u16, last_ephemeral: u16) -> Self {
        Self {
            owners: RwLock::new(BTreeMap::new()),
            first_ephemeral,
            last_ephemeral,
        }
    }

    pub fn claim(&self, family: AddressFamily, requested: u16, owner: SocketId) -> Result<u16, SocketError> {
        let key = match family {
            AddressFamily::Inet4 => AddressFamilyKey::Inet4,
            AddressFamily::Inet6 => AddressFamilyKey::Inet6,
            AddressFamily::Unix => return Err(SocketError::InvalidTransition),
        };
        let mut owners = self.owners.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if requested != 0 {
            match owners.entry((key, requested)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(owner);
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(SocketError::AddressInUse);
                }
            }
            return Ok(requested);
        }
        for port in self.first_ephemeral..=self.last_ephemeral {
            if let std::collections::btree_map::Entry::Vacant(entry) = owners.entry((key, port)) {
                entry.insert(owner);
                return Ok(port);
            }
        }
        Err(SocketError::Capacity)
    }

    pub fn release(&self, family: AddressFamily, port: u16, owner: SocketId) {
        let key = match family {
            AddressFamily::Inet4 => AddressFamilyKey::Inet4,
            AddressFamily::Inet6 => AddressFamilyKey::Inet6,
            AddressFamily::Unix => return,
        };
        let mut owners = self.owners.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners.get(&(key, port)) == Some(&owner) {
            owners.remove(&(key, port));
        }
    }

    #[must_use]
    pub fn owner(&self, family: AddressFamily, port: u16) -> Option<SocketId> {
        let key = match family {
            AddressFamily::Inet4 => AddressFamilyKey::Inet4,
            AddressFamily::Inet6 => AddressFamilyKey::Inet6,
            AddressFamily::Unix => return None,
        };
        self.owners
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(key, port))
            .copied()
    }
}

