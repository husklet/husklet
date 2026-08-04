use std::collections::BTreeMap;
use std::sync::Mutex;

use super::address::UnixAddress;
use crate::SocketId;

const AUTOBIND_LIMIT: u32 = 0x10_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceError {
    AddressInUse,
    Exhausted,
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Binding {
    identity: SocketId,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathnameResolution {
    Missing,
    Stale,
    Live(SocketId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Entry {
    identity: SocketId,
    generation: u64,
    live: Option<SocketId>,
}

#[derive(Debug, Default)]
struct State {
    next_name: u32,
    next_generation: u64,
    addresses: BTreeMap<UnixAddress, Entry>,
}

/// Process-network-namespace ownership for pathname and abstract Unix addresses.
#[derive(Debug, Default)]
pub struct Namespace {
    state: Mutex<State>,
}

impl Namespace {
    pub fn bind(&self, requested: UnixAddress, identity: SocketId) -> Result<(UnixAddress, Binding), NamespaceError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let address = match requested {
            UnixAddress::Unnamed => Self::autobind(&mut state)?,
            UnixAddress::Pathname(ref value) | UnixAddress::Abstract(ref value) if value.is_empty() => {
                return Err(NamespaceError::Invalid);
            }
            address => address,
        };
        if state.addresses.contains_key(&address) {
            return Err(NamespaceError::AddressInUse);
        }
        state.next_generation = state.next_generation.checked_add(1).ok_or(NamespaceError::Exhausted)?;
        let binding = Binding {
            identity,
            generation: state.next_generation,
        };
        state.addresses.insert(
            address.clone(),
            Entry {
                identity,
                generation: binding.generation,
                live: Some(identity),
            },
        );
        Ok((address, binding))
    }

    #[must_use]
    pub fn resolve(&self, address: &UnixAddress) -> Option<SocketId> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .addresses
            .get(address)
            .and_then(|entry| entry.live)
    }

    /// Resolves a pathname independently of the lifetime of its bound endpoint.
    #[must_use]
    pub fn resolve_pathname(&self, pathname: &[u8]) -> PathnameResolution {
        let address = UnixAddress::Pathname(pathname.to_vec());
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match state.addresses.get(&address) {
            None => PathnameResolution::Missing,
            Some(Entry { live: None, .. }) => PathnameResolution::Stale,
            Some(Entry {
                live: Some(identity), ..
            }) => PathnameResolution::Live(*identity),
        }
    }

    /// Returns the generation token for a linked pathname entry.
    #[must_use]
    pub fn pathname_binding(&self, pathname: &[u8]) -> Option<Binding> {
        let address = UnixAddress::Pathname(pathname.to_vec());
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.addresses.get(&address).map(|entry| Binding {
            identity: entry.identity,
            generation: entry.generation,
        })
    }

    /// Releases an endpoint. Abstract names disappear; pathname entries become stale.
    pub fn release(&self, address: &UnixAddress, binding: Binding) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let matches = state
            .addresses
            .get(address)
            .is_some_and(|entry| entry.generation == binding.generation && entry.live == Some(binding.identity));
        if !matches {
            return;
        }
        if matches!(address, UnixAddress::Pathname(_)) {
            state
                .addresses
                .get_mut(address)
                .expect("matched entry must remain present")
                .live = None;
        } else {
            state.addresses.remove(address);
        }
    }

    /// Removes a pathname directory entry without affecting its live endpoint.
    ///
    /// The binding generation prevents a delayed unlink from removing a replacement.
    pub fn unlink_pathname(&self, pathname: &[u8], binding: Binding) -> bool {
        let address = UnixAddress::Pathname(pathname.to_vec());
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let matches = state
            .addresses
            .get(&address)
            .is_some_and(|entry| entry.generation == binding.generation);
        if matches {
            state.addresses.remove(&address);
        }
        matches
    }

    fn autobind(state: &mut State) -> Result<UnixAddress, NamespaceError> {
        for _ in 0..AUTOBIND_LIMIT {
            let candidate = format!("{:05x}", state.next_name).into_bytes();
            state.next_name = (state.next_name + 1) % AUTOBIND_LIMIT;
            let address = UnixAddress::Abstract(candidate);
            if !state.addresses.contains_key(&address) {
                return Ok(address);
            }
        }
        Err(NamespaceError::Exhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::{Binding, Namespace, NamespaceError, PathnameResolution, UnixAddress};
    use crate::SocketId;

    fn id(slot: u16) -> SocketId {
        SocketId { slot, generation: 1 }
    }

    #[test]
    fn collisions_and_release() {
        let namespace = Namespace::default();
        let address = UnixAddress::Abstract(b"service".to_vec());
        let (_, binding) = namespace.bind(address.clone(), id(7)).unwrap();
        assert_eq!(namespace.resolve(&address), Some(id(7)));
        assert_eq!(
            namespace.bind(address.clone(), id(9)),
            Err(NamespaceError::AddressInUse)
        );
        namespace.release(&address, binding);
        assert_eq!(namespace.resolve(&address), None);
        assert!(namespace.bind(address, id(9)).is_ok());
    }

    #[test]
    fn autobind_is_private() {
        let namespace = Namespace::default();
        let (first, _) = namespace.bind(UnixAddress::Unnamed, id(3)).unwrap();
        let (second, _) = namespace.bind(UnixAddress::Unnamed, id(5)).unwrap();
        assert_eq!(first, UnixAddress::Abstract(b"00000".to_vec()));
        assert_eq!(second, UnixAddress::Abstract(b"00001".to_vec()));
    }

    #[test]
    fn stale_release() {
        let namespace = Namespace::default();
        let address = UnixAddress::Pathname(b"/run/service".to_vec());
        let (_, binding) = namespace.bind(address.clone(), id(11)).unwrap();
        namespace.release(
            &address,
            Binding {
                identity: id(11),
                generation: binding.generation + 1,
            },
        );
        assert_eq!(namespace.resolve(&address), Some(id(11)));
    }

    #[test]
    fn pathname_collision_includes_stale_entry() {
        let namespace = Namespace::default();
        let pathname = b"/run/service";
        let address = UnixAddress::Pathname(pathname.to_vec());
        let (_, binding) = namespace.bind(address.clone(), id(1)).unwrap();

        assert_eq!(
            namespace.bind(address.clone(), id(2)),
            Err(NamespaceError::AddressInUse)
        );
        namespace.release(&address, binding);
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Stale);
        assert_eq!(namespace.bind(address, id(2)), Err(NamespaceError::AddressInUse));
    }

    #[test]
    fn pathname_unlink_while_live_permits_rebind() {
        let namespace = Namespace::default();
        let pathname = b"/run/service";
        let address = UnixAddress::Pathname(pathname.to_vec());
        let (_, old) = namespace.bind(address.clone(), id(3)).unwrap();

        assert!(namespace.unlink_pathname(pathname, old));
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Missing);
        let (_, replacement) = namespace.bind(address.clone(), id(4)).unwrap();
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Live(id(4)));

        // Closing the unlinked endpoint cannot disturb its replacement.
        namespace.release(&address, old);
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Live(id(4)));
        namespace.release(&address, replacement);
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Stale);
    }

    #[test]
    fn delayed_unlink_cannot_remove_replacement() {
        let namespace = Namespace::default();
        let pathname = b"/run/service";
        let address = UnixAddress::Pathname(pathname.to_vec());
        let (_, old) = namespace.bind(address.clone(), id(5)).unwrap();
        assert!(namespace.unlink_pathname(pathname, old));
        let (_, replacement) = namespace.bind(address, id(6)).unwrap();

        assert!(!namespace.unlink_pathname(pathname, old));
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Live(id(6)));
        assert!(namespace.unlink_pathname(pathname, replacement));
        assert_eq!(namespace.resolve_pathname(pathname), PathnameResolution::Missing);
    }
}
