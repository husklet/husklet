// The wire modules mirror the whole task-registry vocabulary they serialize.
#![allow(clippy::wildcard_imports)]

use super::*;

impl ProcessSignalWire {
    pub(super) fn from_value(value: &SignalProcessSnapshot) -> Self {
        Self {
            actions: value.actions.iter().map(ActionWire::from_value).collect(),
            pending: value.pending.iter().map(SignalWire::from_value).collect(),
        }
    }
    pub(super) fn into_value(self) -> Result<SignalProcessSnapshot, ()> {
        Ok(SignalProcessSnapshot {
            actions: self
                .actions
                .into_iter()
                .map(ActionWire::into_value)
                .collect::<Result<_, _>>()?,
            pending: self
                .pending
                .into_iter()
                .map(SignalWire::into_value)
                .collect::<Result<_, _>>()?,
        })
    }
}
impl ThreadSignalWire {
    pub(super) fn from_value(value: &SignalThreadSnapshot) -> Self {
        Self {
            mask: value.mask.bits(),
            stack: StackWire::from_value(value.alternate_stack),
            pending: value.pending.iter().map(SignalWire::from_value).collect(),
            deferred: value.deferred.bits(),
            frames: value
                .frames
                .iter()
                .map(|frame| [frame.deferred.bits(), frame.stack_pointer])
                .collect(),
        }
    }
    pub(super) fn into_value(self) -> Result<SignalThreadSnapshot, ()> {
        Ok(SignalThreadSnapshot {
            mask: SignalMask::from_bits(self.mask),
            alternate_stack: self.stack.into_value()?,
            pending: self
                .pending
                .into_iter()
                .map(SignalWire::into_value)
                .collect::<Result<_, _>>()?,
            deferred: SignalMask::from_bits(self.deferred),
            // TaskWire::decode always validates the completed image before it
            // returns, so an oversized vector cannot escape this structural
            // conversion into TaskRegistry::restore.
            frames: self
                .frames
                .into_iter()
                .map(|frame| SignalFrameScope {
                    deferred: SignalMask::from_bits(frame[0]),
                    stack_pointer: frame[1],
                })
                .collect(),
        })
    }
}
impl ActionWire {
    pub(super) fn from_value((signal, action): &(SignalNumber, SignalAction)) -> Self {
        let (disposition, handler) = match action.disposition {
            SignalDisposition::Default => (1, 0),
            SignalDisposition::Ignore => (2, 0),
            SignalDisposition::Handler(handler) => (3, handler),
        };
        Self {
            signal: signal.get(),
            disposition,
            handler,
            flags: action.flags,
            restorer: action.restorer,
            mask: action.mask.bits(),
        }
    }
    pub(super) fn into_value(self) -> Result<(SignalNumber, SignalAction), ()> {
        let disposition = match self.disposition {
            1 if self.handler == 0 => SignalDisposition::Default,
            2 if self.handler == 0 => SignalDisposition::Ignore,
            3 => SignalDisposition::Handler(self.handler),
            _ => return Err(()),
        };
        Ok((
            SignalNumber::new(self.signal).map_err(|_| ())?,
            SignalAction {
                disposition,
                flags: self.flags,
                restorer: self.restorer,
                mask: SignalMask::from_bits(self.mask),
            },
        ))
    }
}
impl StackWire {
    pub(super) fn from_value(value: AlternateStack) -> Self {
        match value {
            AlternateStack::Disabled => Self {
                state: 1,
                pointer: 0,
                size: 0,
            },
            AlternateStack::Enabled { pointer, size } => Self {
                state: 2,
                pointer,
                size,
            },
            AlternateStack::Active { pointer, size } => Self {
                state: 3,
                pointer,
                size,
            },
            AlternateStack::Autodisarm { pointer, size } => Self {
                state: 4,
                pointer,
                size,
            },
        }
    }
    pub(super) fn into_value(self) -> Result<AlternateStack, ()> {
        match self.state {
            1 if self.pointer == 0 && self.size == 0 => Ok(AlternateStack::Disabled),
            2 => Ok(AlternateStack::Enabled {
                pointer: self.pointer,
                size: self.size,
            }),
            3 => Ok(AlternateStack::Active {
                pointer: self.pointer,
                size: self.size,
            }),
            4 => Ok(AlternateStack::Autodisarm {
                pointer: self.pointer,
                size: self.size,
            }),
            _ => Err(()),
        }
    }
}
impl SignalWire {
    pub(super) fn from_value(value: &SignalInfo) -> Self {
        Self {
            signal: value.signal.get(),
            code: value.code,
            error: value.error,
            sender_process: value.sender_process,
            sender_user: value.sender_user,
            value: value.value,
            address: value.address,
            source_tag: value.source_tag,
        }
    }
    pub(super) fn into_value(self) -> Result<SignalInfo, ()> {
        Ok(SignalInfo {
            signal: SignalNumber::new(self.signal).map_err(|_| ())?,
            code: self.code,
            error: self.error,
            sender_process: self.sender_process,
            sender_user: self.sender_user,
            value: self.value,
            address: self.address,
            source_tag: self.source_tag,
        })
    }
}
