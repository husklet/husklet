use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use super::{NetworkCatalog, NetworkCatalogError, CatalogSocket, PortEntry, Slot};
use crate::{NETWORK_CHECKPOINT_VERSION, NetworkCatalogRestore, NetworkCheckpointImage, NetworkSocketState};

impl NetworkCatalog {
    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }

    pub fn checkpoint_image(&self) -> Result<NetworkCheckpointImage, NetworkCatalogError> {
        if !self.activity.frozen() {
            return Err(NetworkCatalogError::Invalid);
        }
        let slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut sockets = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            let Some(socket) = slot.socket.as_deref() else { continue };
            match socket {
                CatalogSocket::Host {
                    snapshot,
                    resource,
                    accepted,
                    ..
                } => sockets.push(NetworkSocketState::Host {
                    snapshot: snapshot.clone(),
                    resource: *resource,
                    accepted: accepted.clone(),
                }),
                CatalogSocket::UnixPair { endpoints, pair } if usize::from(endpoints[0].id.slot) - 1 == index => {
                    sockets.push(NetworkSocketState::UnixPair {
                        endpoints: endpoints.clone(),
                        pair: pair.snapshot(),
                    });
                }
                CatalogSocket::UnixPair { .. } => {}
                CatalogSocket::Unix {
                    snapshot,
                    pending,
                    datagram,
                } => sockets.push(NetworkSocketState::Unix {
                    snapshot: snapshot.clone(),
                    pending: pending.clone(),
                    datagram: datagram.as_ref().map(|socket| socket.snapshot()),
                }),
            }
        }
        let mut ports = self
            .ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|entry| match entry {
                PortEntry::Published(checkpoint) => Some(checkpoint.clone()),
                PortEntry::Prepared { .. } => None,
            })
            .collect::<Vec<_>>();
        ports.sort_by_key(|port| (port.family as u8, port.port));
        let image = NetworkCheckpointImage {
            version: NETWORK_CHECKPOINT_VERSION,
            generations: slots.iter().map(|slot| slot.generation).collect(),
            configuration: self.configuration.clone(),
            ports,
            authority: Vec::new(),
            sockets,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn restore_checkpoint(
        image: &NetworkCheckpointImage,
        restore: &mut dyn NetworkCatalogRestore,
    ) -> Result<Self, NetworkCatalogError> {
        image.validate()?;
        let mut slots = image
            .generations
            .iter()
            .map(|generation| Slot {
                generation: *generation,
                socket: None,
            })
            .collect::<Vec<_>>();
        for socket in &image.sockets {
            Self::restore_socket(socket, &mut slots, restore)?;
        }
        Ok(Self {
            configuration: image.configuration.clone(),
            ports: Mutex::new(image.ports.iter().cloned().map(PortEntry::Published).collect()),
            slots: Mutex::new(slots),
            generation: AtomicU64::new(1),
            port_generation: AtomicU64::new(1),
            activity: crate::checkpoint_activity::CheckpointActivity::default(),
        })
    }

    fn restore_socket(
        state: &NetworkSocketState,
        slots: &mut [Slot],
        restore: &mut dyn NetworkCatalogRestore,
    ) -> Result<(), NetworkCatalogError> {
        let object = match state {
            NetworkSocketState::Host {
                snapshot,
                resource,
                accepted,
            } => {
                let binding = restore.host_socket(snapshot, *resource)?;
                for item in accepted {
                    restore.accepted_socket(item)?;
                }
                Arc::new(CatalogSocket::Host {
                    snapshot: snapshot.clone(),
                    resource: *resource,
                    binding,
                    accepted: accepted.clone(),
                })
            }
            NetworkSocketState::UnixPair { endpoints, pair } => Arc::new(CatalogSocket::UnixPair {
                endpoints: endpoints.clone(),
                pair: restore.unix_pair(pair)?,
            }),
            NetworkSocketState::Unix {
                snapshot,
                pending,
                datagram,
            } => Arc::new(CatalogSocket::Unix {
                snapshot: snapshot.clone(),
                pending: pending.clone(),
                datagram: datagram
                    .as_ref()
                    .map(crate::UnixDatagramSocket::restore)
                    .transpose()
                    .map_err(|_| NetworkCatalogError::Invalid)?
                    .map(Arc::new),
            }),
        };
        match state {
            NetworkSocketState::Host { snapshot, .. } => {
                slots[usize::from(snapshot.id.slot) - 1].socket = Some(object);
            }
            NetworkSocketState::UnixPair { endpoints, .. } => {
                for endpoint in endpoints {
                    slots[usize::from(endpoint.id.slot) - 1].socket = Some(object.clone());
                }
            }
            NetworkSocketState::Unix { snapshot, .. } => {
                slots[usize::from(snapshot.id.slot) - 1].socket = Some(object);
            }
        }
        Ok(())
    }
}
