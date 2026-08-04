use std::sync::Arc;

use hl_descriptor::{
    DescriptionIdentity, DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectKind, OpenDescriptionImage,
    OpenFileDescription,
};
use hl_network::SocketId;

use super::{CheckpointHost, ObjectBindings, PendingBinding, PendingSocket, Phase};

const OBJECT_VERSION: u8 = 1;
const OBJECT_BYTES: usize = 11;

impl<H: CheckpointHost> ObjectBindings<H> {
    fn encode(id: SocketId) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(OBJECT_BYTES);
        bytes.push(OBJECT_VERSION);
        bytes.extend_from_slice(&id.slot.to_le_bytes());
        bytes.extend_from_slice(&id.generation.to_le_bytes());
        bytes
    }

    fn decode(description: &OpenDescriptionImage) -> Result<SocketId, DescriptorCheckpointError> {
        if description.object.len() != OBJECT_BYTES || description.object[0] != OBJECT_VERSION {
            return Err(DescriptorCheckpointError::Object);
        }
        let slot = u16::from_le_bytes(description.object[1..3].try_into().unwrap());
        let generation = u64::from_le_bytes(description.object[3..11].try_into().unwrap());
        if slot == 0 || generation == 0 {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(SocketId { slot, generation })
    }
}

impl<H: CheckpointHost> DescriptorObjectCheckpoint for ObjectBindings<H> {
    fn snapshot(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        if object.kind() != ObjectKind::Socket {
            return Err(DescriptorCheckpointError::Object);
        }
        let (_, sockets) = self.sockets.checkpoint_lease();
        let id = sockets
            .iter()
            .find_map(|(key, socket)| (key.identity == identity).then_some(socket.id))
            .ok_or(DescriptorCheckpointError::Object)?;
        Ok(Self::encode(id))
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        if description.kind != ObjectKind::Socket {
            return Err(DescriptorCheckpointError::Object);
        }
        let id = Self::decode(description)?;
        let object = Arc::new(PendingSocket::new());
        let mut state = self.state.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        if state.phase == Phase::Resumed {
            state.phase = Phase::Staging;
        }
        if state.phase != Phase::Staging {
            return Err(DescriptorCheckpointError::Object);
        }
        state.pending.entry(id).or_default().push(PendingBinding {
            identity: DescriptionIdentity {
                identity: description.identity,
                generation: description.generation,
            },
            object: object.clone(),
        });
        Ok(object)
    }
}
