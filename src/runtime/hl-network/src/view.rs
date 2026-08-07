//! Value-only observations of catalog state.
use crate::catalog::{CatalogSocket, NetworkCatalog};
use crate::{SocketId, SocketSnapshot};
use std::sync::atomic::Ordering;

/// One coherent observation of the live sockets in a network namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkNamespaceView {
    /// Changes after every catalog mutation represented by this view.
    pub generation: u64,
    pub unix: Vec<UnixSocketView>,
    pub internet: Vec<InternetSocketView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternetSocketView {
    pub inode: u64,
    pub family: crate::AddressFamily,
    pub socket_type: crate::SocketType,
    pub state: crate::SocketState,
    pub local: Option<crate::SocketAddress>,
    pub peer: Option<crate::SocketAddress>,
}

/// A live `AF_UNIX` socket as observed while holding the catalog lock once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnixSocketView {
    pub id: SocketId,
    pub inode: u64,
    pub socket_type: crate::SocketType,
    pub state: crate::SocketState,
    pub path: Option<Vec<u8>>,
}

impl NetworkCatalog {
    /// Captures every live `AF_UNIX` endpoint from a single catalog state.
    #[must_use]
    pub fn namespace_view(&self) -> NetworkNamespaceView {
        let _admission = self.activity.admit();
        let slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut unix = Vec::new();
        let mut internet = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            let Some(socket) = slot.socket.as_deref() else { continue };
            match socket {
                CatalogSocket::UnixPair { endpoints, .. } => {
                    if let Some(snapshot) = endpoints
                        .iter()
                        .find(|snapshot| usize::from(snapshot.id.slot) - 1 == index)
                    {
                        unix.push(Self::unix_view(snapshot));
                    }
                }
                CatalogSocket::Unix { snapshot, .. } => unix.push(Self::unix_view(snapshot)),
                CatalogSocket::Host { snapshot, .. } if snapshot.family == crate::AddressFamily::Unix => {
                    unix.push(Self::unix_view(snapshot));
                }
                CatalogSocket::Host { snapshot, .. } => internet.push(InternetSocketView {
                    inode: snapshot.id.generation.wrapping_shl(16) | u64::from(snapshot.id.slot),
                    family: snapshot.family,
                    socket_type: snapshot.socket_type,
                    state: snapshot.state,
                    local: snapshot.local.clone(),
                    peer: snapshot.peer.clone(),
                }),
            }
        }
        unix.sort_by_key(|socket| socket.id);
        NetworkNamespaceView {
            generation: self.generation.load(Ordering::Acquire),
            unix,
            internet,
        }
    }

    pub(crate) fn unix_view(snapshot: &SocketSnapshot) -> UnixSocketView {
        let path = match &snapshot.local {
            Some(crate::SocketAddress::Unix(path)) => Some(path.clone()),
            _ => None,
        };
        UnixSocketView {
            id: snapshot.id,
            inode: snapshot.id.generation.wrapping_shl(16) | u64::from(snapshot.id.slot),
            socket_type: snapshot.socket_type,
            state: snapshot.state,
            path,
        }
    }
}
