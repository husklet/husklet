mod codec;
mod queue;
mod snapshot;

pub use codec::{ControlCodec, ControlEncoding};
pub(crate) use queue::RIGHTS_MAXIMUM;
pub use queue::{
    ControlError, ControlMessage, ControlWord, QueueMessageSnapshot, QueueRightsSnapshot, QueueSnapshot,
    ReceiveControl, SenderCredentials, UnixMessageQueue,
};

#[cfg(test)]
mod test;
