use super::{Descriptor, DescriptorInstall, DescriptorSyscalls, HostError, ReceivedDescriptors};
use std::sync::Arc;

const MAXIMUM_FRAME: usize = 1 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkFrame {
    bytes: Vec<u8>,
}

impl ForkFrame {
    pub fn new(bytes: Vec<u8>) -> Result<Self, ForkWireError> {
        (!bytes.is_empty() && bytes.len() <= MAXIMUM_FRAME)
            .then_some(Self { bytes })
            .ok_or(ForkWireError::InvalidFrame)
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForkWireError {
    Host(HostError),
    InvalidFrame,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCredentials {
    pub process: u32,
    pub user: u32,
    pub group: u32,
}

pub struct AttachmentFrame<S: DescriptorSyscalls> {
    pub frame: ForkFrame,
    pub credentials: Option<PeerCredentials>,
    pub descriptors: ReceivedDescriptors<S>,
}

pub trait ForkWireSyscalls: Send + Sync {
    fn close_channel(&self, channel: u64);
    fn send(&self, channel: u64, bytes: &[u8]) -> Result<usize, HostError>;
    fn receive(&self, channel: u64, bytes: &mut [u8]) -> Result<usize, HostError>;
    fn send_attachments(&self, _channel: u64, _bytes: &[u8], _descriptors: &[i32]) -> Result<usize, HostError> {
        Err(HostError::Unsupported)
    }
    fn receive_attachments(
        &self,
        _channel: u64,
        _bytes: &mut [u8],
        _capacity: usize,
    ) -> Result<(usize, Vec<i32>, Option<PeerCredentials>), HostError> {
        Err(HostError::Unsupported)
    }
}

pub struct ChildChannel<S: ForkWireSyscalls + DescriptorSyscalls> {
    syscalls: Arc<S>,
    channel: Option<u64>,
    send: Vec<u8>,
    sent: usize,
    receive: Vec<u8>,
    expected: Option<usize>,
    attachments: Vec<Descriptor<S>>,
}

impl<S: ForkWireSyscalls + DescriptorSyscalls> ChildChannel<S> {
    #[must_use]
    pub fn from_host_handle(syscalls: Arc<S>, channel: u64) -> Self {
        Self {
            syscalls,
            channel: Some(channel),
            send: Vec::new(),
            sent: 0,
            receive: Vec::new(),
            expected: None,
            attachments: Vec::new(),
        }
    }

    pub fn begin_send(&mut self, frame: &ForkFrame) -> Result<(), ForkWireError> {
        if self.sent != self.send.len() {
            return Err(ForkWireError::Host(HostError::WouldBlock));
        }
        let length = u32::try_from(frame.bytes.len()).map_err(|_| ForkWireError::InvalidFrame)?;
        self.send.clear();
        self.send.extend_from_slice(&length.to_le_bytes());
        self.send.extend_from_slice(&frame.bytes);
        self.sent = 0;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<bool, ForkWireError> {
        while self.sent < self.send.len() {
            let result = if self.sent == 0 && !self.attachments.is_empty() {
                let descriptors: Vec<i32> = self.attachments.iter().map(Descriptor::raw).collect();
                self.syscalls.send_attachments(self.handle()?, &self.send, &descriptors)
            } else {
                self.syscalls.send(self.handle()?, &self.send[self.sent..])
            };
            match result {
                Ok(0) => return Err(ForkWireError::Host(HostError::WouldBlock)),
                Ok(count) if count <= self.send.len() - self.sent => self.sent += count,
                Ok(_) => return Err(ForkWireError::Host(HostError::Failed)),
                Err(HostError::Interrupted) => {}
                Err(error) => return Err(ForkWireError::Host(error)),
            }
            if self.sent > 0 {
                self.attachments.clear();
            }
        }
        Ok(true)
    }

    pub fn cancel_send(&mut self) {
        self.send.clear();
        self.sent = 0;
        self.attachments.clear();
    }

    pub fn receive(&mut self) -> Result<Option<ForkFrame>, ForkWireError> {
        loop {
            if let Some(frame) = self.completed_frame()? {
                return Ok(Some(frame));
            }
            let target = self.expected.map_or(4, |length| length + 4);
            let mut bytes = [0; 4096];
            let capacity = (target - self.receive.len()).min(bytes.len());
            match self.syscalls.receive(self.handle()?, &mut bytes[..capacity]) {
                Ok(0) => return Err(ForkWireError::Closed),
                Ok(count) if count <= capacity => self.receive.extend_from_slice(&bytes[..count]),
                Ok(_) => return Err(ForkWireError::Host(HostError::Failed)),
                Err(HostError::Interrupted) => continue,
                Err(error) => return Err(ForkWireError::Host(error)),
            }
            self.accept_header()?;
        }
    }

    fn accept_header(&mut self) -> Result<(), ForkWireError> {
        if self.expected.is_some() || self.receive.len() != 4 {
            return Ok(());
        }
        let length = u32::from_le_bytes([self.receive[0], self.receive[1], self.receive[2], self.receive[3]]) as usize;
        if length == 0 || length > MAXIMUM_FRAME {
            return Err(ForkWireError::InvalidFrame);
        }
        self.expected = Some(length);
        Ok(())
    }

    fn completed_frame(&mut self) -> Result<Option<ForkFrame>, ForkWireError> {
        let Some(expected) = self.expected else {
            return Ok(None);
        };
        if self.receive.len() != expected + 4 {
            return Ok(None);
        }
        let bytes = self.receive.split_off(4);
        self.receive.clear();
        self.expected = None;
        ForkFrame::new(bytes).map(Some)
    }

    fn handle(&self) -> Result<u64, ForkWireError> {
        self.channel.ok_or(ForkWireError::Closed)
    }
}

impl<S: ForkWireSyscalls + DescriptorSyscalls> ChildChannel<S> {
    pub fn send_with_descriptors(
        &mut self,
        frame: &ForkFrame,
        descriptors: &[&Descriptor<S>],
    ) -> Result<bool, ForkWireError> {
        if descriptors.len() > 8 {
            return Err(ForkWireError::InvalidFrame);
        }
        if self.sent != self.send.len() {
            return Err(ForkWireError::Host(HostError::WouldBlock));
        }
        let mut retained = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let raw = self
                .syscalls
                .duplicate_cloexec(descriptor.raw(), 3)
                .map_err(ForkWireError::Host)?;
            retained.push(Descriptor::from_raw(Arc::clone(&self.syscalls), raw).map_err(ForkWireError::Host)?);
        }
        self.begin_send(frame)?;
        self.attachments = retained;
        self.flush()
    }

    pub fn receive_with_descriptors(&self, capacity: usize) -> Result<AttachmentFrame<S>, ForkWireError> {
        if capacity > 8 {
            return Err(ForkWireError::InvalidFrame);
        }
        let mut bytes = vec![0_u8; MAXIMUM_FRAME + 4];
        let (count, raw, credentials) = self
            .syscalls
            .receive_attachments(self.handle()?, &mut bytes, capacity)
            .map_err(ForkWireError::Host)?;
        if count < 4 || count > bytes.len() || raw.len() > capacity {
            for descriptor in raw {
                self.syscalls.close_descriptor(descriptor);
            }
            return Err(ForkWireError::InvalidFrame);
        }
        let expected = u32::from_le_bytes(bytes[..4].try_into().map_err(|_| ForkWireError::InvalidFrame)?) as usize;
        if expected == 0 || expected > MAXIMUM_FRAME || expected + 4 != count {
            for descriptor in raw {
                self.syscalls.close_descriptor(descriptor);
            }
            return Err(ForkWireError::InvalidFrame);
        }
        let descriptors = raw
            .into_iter()
            .map(|raw| Descriptor::from_raw(Arc::clone(&self.syscalls), raw))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ForkWireError::Host)?;
        Ok(AttachmentFrame {
            frame: ForkFrame::new(bytes[4..count].to_vec())?,
            credentials,
            descriptors: ReceivedDescriptors::new(descriptors),
        })
    }

    pub fn receive_and_install<I: DescriptorInstall<S>>(
        &self,
        capacity: usize,
        installer: &mut I,
    ) -> Result<(ForkFrame, Option<PeerCredentials>), ForkWireError> {
        let received = self.receive_with_descriptors(capacity)?;
        received.descriptors.install(installer).map_err(ForkWireError::Host)?;
        Ok((received.frame, received.credentials))
    }
}

impl<S: ForkWireSyscalls + DescriptorSyscalls> Drop for ChildChannel<S> {
    fn drop(&mut self) {
        if let Some(channel) = self.channel.take() {
            self.syscalls.close_channel(channel);
        }
    }
}
