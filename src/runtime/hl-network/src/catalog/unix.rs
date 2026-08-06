use super::{NetworkCatalog, SocketId, SocketSnapshot, Arc, UnixSocketPair, NetworkCatalogError, CatalogSocket, Slot, NETWORK_CHECKPOINT_SOCKET_MAXIMUM};

impl NetworkCatalog {
    pub fn connect_unix_pair(
        &self,
        listener: SocketId,
        client: SocketId,
        client_snapshot: SocketSnapshot,
        mut accepted_snapshot: SocketSnapshot,
        pair: Arc<UnixSocketPair>,
    ) -> Result<SocketId, NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (listener_snapshot, listener_pending) = match Self::slot(&slots, listener)?.socket.as_deref() {
            Some(CatalogSocket::Unix { snapshot, pending, .. }) => (snapshot.clone(), pending.clone()),
            Some(_) => return Err(NetworkCatalogError::Invalid),
            None => return Err(NetworkCatalogError::Stale),
        };
        let crate::SocketState::Listening { backlog } = listener_snapshot.state else {
            return Err(NetworkCatalogError::Invalid);
        };
        let current = match Self::slot(&slots, client)?.socket.as_deref() {
            Some(CatalogSocket::Unix { snapshot, .. }) => snapshot,
            Some(_) => return Err(NetworkCatalogError::Invalid),
            None => return Err(NetworkCatalogError::Stale),
        };
        if client_snapshot.id != client
            || current.family != crate::AddressFamily::Unix
            || client_snapshot.family != crate::AddressFamily::Unix
            || accepted_snapshot.family != crate::AddressFamily::Unix
            || client_snapshot.socket_type != current.socket_type
            || client_snapshot.protocol != current.protocol
            || accepted_snapshot.socket_type != current.socket_type
            || accepted_snapshot.protocol != current.protocol
            || pair.socket_type() != current.socket_type
            || listener_snapshot.family != crate::AddressFamily::Unix
            || listener == client
            || u32::try_from(listener_pending.len()).map_or(true, |count| count >= backlog)
            || listener_snapshot.socket_type != current.socket_type
            || listener_snapshot.protocol != current.protocol
            || !matches!(current.state, crate::SocketState::Created | crate::SocketState::Bound)
            || current.state == crate::SocketState::Bound && client_snapshot.local != current.local
        {
            return Err(NetworkCatalogError::Invalid);
        }
        let (accepted_index, accepted_generation) = Self::allocation_candidate(&slots)?;
        let accepted = SocketId {
            slot: u16::try_from(accepted_index + 1).map_err(|_| NetworkCatalogError::Capacity)?,
            generation: accepted_generation,
        };
        accepted_snapshot.id = accepted;
        let endpoints = [client_snapshot, accepted_snapshot];
        if endpoints
            .iter()
            .any(|snapshot| !crate::SocketNamespace::valid_checkpoint_snapshot(snapshot))
            || endpoints
                .iter()
                .any(|snapshot| snapshot.state != crate::SocketState::Connected)
            || endpoints[0].peer != endpoints[1].local
            || endpoints[1].peer != endpoints[0].local
            || endpoints[1].local != listener_snapshot.local
        {
            return Err(NetworkCatalogError::Invalid);
        }
        if accepted_index == slots.len() {
            slots.push(Slot {
                generation: accepted_generation,
                socket: None,
            });
        } else {
            slots[accepted_index].generation = accepted_generation;
        }
        let object = Arc::new(CatalogSocket::UnixPair { endpoints, pair });
        slots[usize::from(client.slot) - 1].socket = Some(object.clone());
        slots[accepted_index].socket = Some(object);
        let mut pending = listener_pending;
        pending.push(accepted);
        slots[usize::from(listener.slot) - 1].socket = Some(Arc::new(CatalogSocket::Unix {
            snapshot: listener_snapshot,
            pending,
            datagram: None,
        }));
        self.advance_generation();
        Ok(accepted)
    }

    pub fn accept_pending_unix(&self, listener: SocketId) -> Result<SocketId, NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (snapshot, mut pending) = match Self::slot(&slots, listener)?.socket.as_deref() {
            Some(CatalogSocket::Unix { snapshot, pending, .. })
                if matches!(snapshot.state, crate::SocketState::Listening { .. }) =>
            {
                (snapshot.clone(), pending.clone())
            }
            Some(CatalogSocket::Unix { .. } | _) => return Err(NetworkCatalogError::Invalid),
            None => return Err(NetworkCatalogError::Stale),
        };
        let accepted = pending.first().copied().ok_or(NetworkCatalogError::Stale)?;
        if !matches!(
            Self::slot(&slots, accepted)?.socket.as_deref(),
            Some(CatalogSocket::UnixPair { .. })
        ) {
            return Err(NetworkCatalogError::Invalid);
        }
        pending.remove(0);
        slots[usize::from(listener.slot) - 1].socket = Some(Arc::new(CatalogSocket::Unix {
            snapshot,
            pending,
            datagram: None,
        }));
        self.advance_generation();
        Ok(accepted)
    }

    fn allocation_candidate(slots: &[Slot]) -> Result<(usize, u64), NetworkCatalogError> {
        let index = slots
            .iter()
            .position(|slot| slot.socket.is_none())
            .unwrap_or(slots.len());
        if index >= NETWORK_CHECKPOINT_SOCKET_MAXIMUM {
            return Err(NetworkCatalogError::Capacity);
        }
        let generation = slots
            .get(index)
            .map_or(Some(1), |slot| slot.generation.checked_add(1))
            .ok_or(NetworkCatalogError::Capacity)?;
        Ok((index, generation))
    }
}
