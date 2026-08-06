//! In-memory socket slot table and the host contract its operations run against.
use crate::{
    AddressFamily, SOCKET_MAXIMUM, ShutdownState, SocketAddress, SocketConnectError, SocketError, SocketId,
    SocketProtocol, SocketSnapshot, SocketState, SocketType,
};
use std::sync::RwLock;
#[derive(Default)]
struct Slot {
    generation: u64,
    socket: Option<SocketSnapshot>,
}

pub struct SocketNamespace {
    slots: RwLock<Vec<Slot>>,
}

impl SocketNamespace {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: RwLock::new(Vec::new()),
        }
    }

    pub fn create(
        &self,
        family: AddressFamily,
        socket_type: SocketType,
        protocol: SocketProtocol,
        nonblocking: bool,
    ) -> Result<SocketId, SocketError> {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = slots
            .iter()
            .position(|slot| slot.socket.is_none())
            .unwrap_or(slots.len());
        if index == SOCKET_MAXIMUM {
            return Err(SocketError::Capacity);
        }
        if index == slots.len() {
            slots.push(Slot::default());
        }
        let slot = &mut slots[index];
        slot.generation = slot.generation.checked_add(1).ok_or(SocketError::Capacity)?;
        let id = SocketId {
            slot: u16::try_from(index + 1).expect("bounded socket slot"),
            generation: slot.generation,
        };
        slot.socket = Some(SocketSnapshot {
            id,
            family,
            socket_type,
            protocol,
            state: SocketState::Created,
            local: None,
            peer: None,
            connect_error: None,
            nonblocking,
            shutdown: ShutdownState::default(),
        });
        Ok(id)
    }

    pub fn update(&self, id: SocketId, operation: SocketOperation) -> Result<(), SocketError> {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let socket = Self::socket_mut(&mut slots, id)?;
        match operation {
            SocketOperation::Bind(address) if socket.state == SocketState::Created => {
                socket.local = Some(address);
                socket.state = SocketState::Bound;
            }
            SocketOperation::Listen(backlog)
                if socket.socket_type == SocketType::Stream && matches!(socket.state, SocketState::Bound) =>
            {
                socket.state = SocketState::Listening { backlog };
            }
            SocketOperation::BeginConnect(address)
                if matches!(socket.state, SocketState::Created | SocketState::Bound) =>
            {
                socket.peer = Some(address);
                socket.state = SocketState::Connecting;
            }
            SocketOperation::FinishConnect if socket.state == SocketState::Connecting => {
                socket.state = SocketState::Connected;
            }
            SocketOperation::Shutdown(shutdown) if socket.state == SocketState::Connected => {
                socket.shutdown.read |= shutdown.read;
                socket.shutdown.write |= shutdown.write;
            }
            SocketOperation::SetNonblocking(value) => socket.nonblocking = value,
            _ => return Err(SocketError::InvalidTransition),
        }
        Ok(())
    }

    pub fn close(&self, id: SocketId) -> Result<(), SocketError> {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let socket = Self::socket_mut(&mut slots, id)?;
        socket.state = SocketState::Closed;
        let index = usize::from(id.slot) - 1;
        slots[index].socket = None;
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self, id: SocketId) -> Option<SocketSnapshot> {
        let slots = self.slots.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = usize::from(id.slot).checked_sub(1)?;
        slots
            .get(index)?
            .socket
            .as_ref()
            .filter(|socket| socket.id == id)
            .cloned()
    }

    pub fn restore(snapshots: &[SocketSnapshot]) -> Result<Self, SocketError> {
        if snapshots.len() > SOCKET_MAXIMUM {
            return Err(SocketError::Capacity);
        }
        let mut slots = Vec::new();
        for snapshot in snapshots {
            let index = usize::from(snapshot.id.slot)
                .checked_sub(1)
                .ok_or(SocketError::InvalidTransition)?;
            if index >= SOCKET_MAXIMUM || snapshot.id.generation == 0 || !Self::valid_checkpoint_snapshot(snapshot) {
                return Err(SocketError::InvalidTransition);
            }
            slots.resize_with(index + 1, Slot::default);
            if slots[index].generation != 0 || slots[index].socket.is_some() {
                return Err(SocketError::InvalidTransition);
            }
            slots[index] = Slot {
                generation: snapshot.id.generation,
                socket: Some(snapshot.clone()),
            };
        }
        Ok(Self {
            slots: RwLock::new(slots),
        })
    }

    pub(crate) fn valid_checkpoint_snapshot(snapshot: &SocketSnapshot) -> bool {
        let valid_state = match snapshot.state {
            SocketState::Created => snapshot.local.is_none() && snapshot.peer.is_none(),
            SocketState::Bound => snapshot.local.is_some() && snapshot.peer.is_none(),
            SocketState::Listening { .. } => {
                matches!(snapshot.socket_type, SocketType::Stream | SocketType::SequencePacket)
                    && snapshot.local.is_some()
                    && snapshot.peer.is_none()
            }
            SocketState::Connecting => snapshot.peer.is_some(),
            SocketState::Connected => snapshot.peer.is_some(),
            SocketState::Closed => false,
        };
        valid_state
            && !matches!(
                snapshot.connect_error,
                Some(SocketConnectError::InProgress | SocketConnectError::Already | SocketConnectError::Connected)
            )
            && (snapshot.connect_error.is_none() || snapshot.state == SocketState::Connecting)
    }

    fn socket_mut(slots: &mut [Slot], id: SocketId) -> Result<&mut SocketSnapshot, SocketError> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(SocketError::Stale)?;
        slots
            .get_mut(index)
            .ok_or(SocketError::Stale)?
            .socket
            .as_mut()
            .filter(|socket| socket.id == id)
            .ok_or(SocketError::Stale)
    }
}

impl Default for SocketNamespace {
    fn default() -> Self {
        Self::new()
    }
}

pub enum SocketOperation {
    Bind(SocketAddress),
    Listen(u32),
    BeginConnect(SocketAddress),
    FinishConnect,
    Shutdown(ShutdownState),
    SetNonblocking(bool),
}

pub trait SocketHost {
    type Token;
    type Error;

    fn open(&self, snapshot: &SocketSnapshot) -> Result<Self::Token, Self::Error>;
}
