use crate::{QueueSnapshot, SenderCredentials, SocketType, UnixAddress, UnixTransportError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointSnapshot {
    pub address: UnixAddress,
    pub incoming: Vec<Vec<u8>>,
    pub peer_write_shutdown: bool,
    pub read_shutdown: bool,
    pub write_shutdown: bool,
    pub closed: bool,
    pub passcred: bool,
    pub peer_credentials: Option<SenderCredentials>,
    pub ancillary: QueueSnapshot,
}
pub type UnixEndpointSnapshot = EndpointSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairSnapshot {
    pub socket_type: SocketType,
    pub capacity: usize,
    pub endpoints: [UnixEndpointSnapshot; 2],
}
pub type UnixPairSnapshot = PairSnapshot;

impl UnixPairSnapshot {
    pub fn validate(&self) -> Result<(), UnixTransportError> {
        if self.capacity == 0
            || !matches!(
                self.socket_type,
                SocketType::Stream | SocketType::Datagram | SocketType::SequencePacket
            )
        {
            return Err(UnixTransportError::Invalid);
        }
        for endpoint in &self.endpoints {
            endpoint.ancillary.validate().map_err(UnixTransportError::Control)?;
            let incoming = endpoint.incoming.iter().try_fold(0_usize, |size, record| {
                size.checked_add(record.len()).ok_or(UnixTransportError::Invalid)
            })?;
            let ancillary = endpoint.ancillary.messages.iter().try_fold(0_usize, |size, message| {
                size.checked_add(message.payload.len())
                    .ok_or(UnixTransportError::Invalid)
            })?;
            let buffered = incoming.checked_add(ancillary).ok_or(UnixTransportError::Invalid)?;
            if buffered > self.capacity {
                return Err(UnixTransportError::Invalid);
            }
        }
        Ok(())
    }
}
