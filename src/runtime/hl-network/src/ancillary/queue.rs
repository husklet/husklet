use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptionRef, DescriptorError, DescriptorFlags, DescriptorTable};
use hl_sync::WaitQueue;

pub(crate) const RIGHTS_MAXIMUM: usize = 253;
pub(super) const QUEUE_MESSAGE_MAXIMUM: usize = 1024;
pub(super) const QUEUE_DATA_MAXIMUM: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlWord {
    Four,
    Eight,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlMessage {
    Rights(Vec<i32>),
    Credentials { process: u32, user: u32, group: u32 },
    Unknown { level: i32, kind: i32, data: Vec<u8> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SenderCredentials {
    pub process: u32,
    pub user: u32,
    pub group: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    Invalid,
    TooBig,
    BadDescriptor,
    TooManyOpenFiles,
    Fault,
    Canceled,
    PermissionDenied,
    MissingDescription,
}

pub(super) struct QueuedRights {
    pub(super) rights: Vec<DescriptionRef>,
}

pub(super) struct UnixMessage {
    pub(super) payload: Vec<u8>,
    pub(super) controls: Vec<ControlMessage>,
    pub(super) rights: Vec<QueuedRights>,
    pub(super) credentials: Option<SenderCredentials>,
    pub(super) automatic: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiveControl {
    pub descriptors: Vec<i32>,
    pub truncated: bool,
    pub controls: Vec<ControlMessage>,
}

pub struct UnixMessageQueue {
    pub(super) messages: Mutex<VecDeque<UnixMessage>>,
    pub(super) wait: Arc<WaitQueue>,
    pub(super) passcred: AtomicBool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueRightsSnapshot {
    pub identities: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueMessageSnapshot {
    pub payload: Vec<u8>,
    pub controls: Vec<ControlMessage>,
    pub rights: Vec<QueueRightsSnapshot>,
    pub credentials: Option<SenderCredentials>,
    pub automatic: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSnapshot {
    pub messages: Vec<QueueMessageSnapshot>,
}

impl UnixMessageQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            messages: Mutex::new(VecDeque::new()),
            wait: Arc::new(WaitQueue::new()),
            passcred: AtomicBool::new(false),
        }
    }

    pub fn send(
        &self,
        sender: &DescriptorTable,
        payload: Vec<u8>,
        controls: Vec<ControlMessage>,
    ) -> Result<(), ControlError> {
        self.send_authenticated(sender, payload, controls, None)
    }

    pub fn send_authenticated(
        &self,
        sender: &DescriptorTable,
        payload: Vec<u8>,
        controls: Vec<ControlMessage>,
        authenticated: Option<SenderCredentials>,
    ) -> Result<(), ControlError> {
        if payload.len() > QUEUE_DATA_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        Self::validate_rights_total(&controls)?;
        let mut rights = Vec::new();
        for control in &controls {
            Self::validate_credentials(control, authenticated)?;
            if let Some(exported) = Self::export_control(sender, control)? {
                rights.push(QueuedRights { rights: exported });
            }
        }
        let mut messages = self.messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if messages.len() >= QUEUE_MESSAGE_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        let explicit = controls
            .iter()
            .any(|control| matches!(control, ControlMessage::Credentials { .. }));
        messages.push_back(UnixMessage {
            payload,
            controls,
            rights,
            credentials: authenticated,
            automatic: self.passcred() && authenticated.is_some() && !explicit,
        });
        drop(messages);
        self.wait.notify_one();
        Ok(())
    }

    fn validate_rights_total(controls: &[ControlMessage]) -> Result<(), ControlError> {
        let total = controls.iter().try_fold(0_usize, |total, control| {
            let count = match control {
                ControlMessage::Rights(numbers) => numbers.len(),
                _ => 0,
            };
            total.checked_add(count).ok_or(ControlError::TooBig)
        })?;
        if total > RIGHTS_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        Ok(())
    }

    #[must_use]
    pub fn wait_queue(&self) -> &WaitQueue {
        &self.wait
    }

    pub(crate) fn wait_handle(&self) -> Arc<WaitQueue> {
        self.wait.clone()
    }

    pub fn set_passcred(&self, enabled: bool) {
        self.passcred.store(enabled, Ordering::Release);
    }

    #[must_use]
    pub fn passcred(&self) -> bool {
        self.passcred.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn has_message(&self) -> bool {
        !self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    fn validate_credentials(
        control: &ControlMessage,
        authenticated: Option<SenderCredentials>,
    ) -> Result<(), ControlError> {
        let ControlMessage::Credentials { process, user, group } = control else {
            return Ok(());
        };
        if authenticated
            != Some(SenderCredentials {
                process: *process,
                user: *user,
                group: *group,
            })
        {
            return Err(ControlError::PermissionDenied);
        }
        Ok(())
    }

    fn export_rights(sender: &DescriptorTable, numbers: &[i32]) -> Result<Vec<DescriptionRef>, ControlError> {
        numbers
            .iter()
            .map(|number| {
                sender
                    .export_description(*number)
                    .map_err(|_| ControlError::BadDescriptor)
            })
            .collect()
    }

    fn export_control(
        sender: &DescriptorTable,
        control: &ControlMessage,
    ) -> Result<Option<Vec<DescriptionRef>>, ControlError> {
        let ControlMessage::Rights(numbers) = control else {
            return Ok(None);
        };
        if numbers.len() > RIGHTS_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        Self::export_rights(sender, numbers).map(Some)
    }

    pub fn receive(
        &self,
        receiver: &DescriptorTable,
        descriptor_capacity: usize,
        close_on_exec: bool,
    ) -> Result<Option<(Vec<u8>, ReceiveControl)>, ControlError> {
        let Some(message) = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        else {
            return Ok(None);
        };
        let controls = Self::effective_controls(&message.controls, message.credentials, message.automatic);
        let rights = message
            .rights
            .into_iter()
            .flat_map(|rights| rights.rights)
            .collect::<Vec<_>>();
        let truncated = rights.len() > descriptor_capacity;
        let delivered = &rights[..rights.len().min(descriptor_capacity)];
        let flags = DescriptorFlags::from_bits(if close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let descriptors = receiver
            .install_descriptions(0, delivered, flags)
            .map_err(|error| match error {
                DescriptorError::TooManyOpenFiles => ControlError::TooManyOpenFiles,
                _ => ControlError::Invalid,
            })?;
        let controls = Self::received_controls(&controls, &descriptors);
        Ok(Some((
            message.payload,
            ReceiveControl {
                descriptors,
                truncated,
                controls,
            },
        )))
    }

    pub fn receive_transactional<F>(
        &self,
        receiver: &DescriptorTable,
        close_on_exec: bool,
        copyout: F,
    ) -> Result<Option<ReceiveControl>, ControlError>
    where
        F: FnOnce(&[u8], &[i32]) -> Result<(), ControlError>,
    {
        self.receive_transactional_capacity(receiver, usize::MAX, close_on_exec, |payload, control| {
            copyout(payload, &control.descriptors)
        })
    }

    pub fn receive_transactional_capacity<F>(
        &self,
        receiver: &DescriptorTable,
        control_capacity: usize,
        close_on_exec: bool,
        copyout: F,
    ) -> Result<Option<ReceiveControl>, ControlError>
    where
        F: FnOnce(&[u8], &ReceiveControl) -> Result<(), ControlError>,
    {
        self.receive_observed(receiver, control_capacity, close_on_exec, |_| {}, copyout)
    }

    pub(crate) fn receive_observed<F, O>(
        &self,
        receiver: &DescriptorTable,
        control_capacity: usize,
        close_on_exec: bool,
        consumed: O,
        copyout: F,
    ) -> Result<Option<ReceiveControl>, ControlError>
    where
        F: FnOnce(&[u8], &ReceiveControl) -> Result<(), ControlError>,
        O: FnOnce(usize),
    {
        let Some(message) = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        else {
            return Ok(None);
        };
        consumed(message.payload.len());
        self.deliver_transactional(message, receiver, control_capacity, close_on_exec, copyout)
            .map(Some)
    }

    pub(crate) fn peek_transactional_capacity<F>(
        &self,
        receiver: &DescriptorTable,
        control_capacity: usize,
        close_on_exec: bool,
        copyout: F,
    ) -> Result<Option<ReceiveControl>, ControlError>
    where
        F: FnOnce(&[u8], &ReceiveControl) -> Result<(), ControlError>,
    {
        let message = self
            .messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .front()
            .map(Self::clone_message);
        let Some(message) = message else {
            return Ok(None);
        };
        self.deliver_transactional(message, receiver, control_capacity, close_on_exec, copyout)
            .map(Some)
    }

    fn clone_message(message: &UnixMessage) -> UnixMessage {
        UnixMessage {
            payload: message.payload.clone(),
            controls: message.controls.clone(),
            rights: message
                .rights
                .iter()
                .map(|group| QueuedRights {
                    rights: group.rights.clone(),
                })
                .collect(),
            credentials: message.credentials,
            automatic: message.automatic,
        }
    }

    fn deliver_transactional<F>(
        &self,
        message: UnixMessage,
        receiver: &DescriptorTable,
        control_capacity: usize,
        close_on_exec: bool,
        copyout: F,
    ) -> Result<ReceiveControl, ControlError>
    where
        F: FnOnce(&[u8], &ReceiveControl) -> Result<(), ControlError>,
    {
        let controls = Self::effective_controls(&message.controls, message.credentials, message.automatic);
        let rights = message
            .rights
            .into_iter()
            .flat_map(|rights| rights.rights)
            .collect::<Vec<_>>();
        let placeholders = Self::received_controls(&controls, &vec![0; rights.len()]);
        let encoding = crate::ControlCodec::encode(&placeholders, ControlWord::Eight, control_capacity.min(65_536))?;
        let fitting = crate::ControlCodec::decode(&encoding.bytes, ControlWord::Eight)?;
        let delivered_count = fitting
            .iter()
            .map(|control| match control {
                ControlMessage::Rights(numbers) => numbers.len(),
                _ => 0,
            })
            .sum::<usize>();
        let flags = DescriptorFlags::from_bits(if close_on_exec {
            DescriptorFlags::CLOSE_ON_EXEC
        } else {
            0
        });
        let transaction = receiver
            .prepare_descriptions(0, &rights[..delivered_count], flags)
            .map_err(|error| match error {
                DescriptorError::TooManyOpenFiles => ControlError::TooManyOpenFiles,
                _ => ControlError::Invalid,
            })?;
        let numbers = transaction.numbers();
        let controls = Self::received_controls(&fitting, &numbers);
        let automatic_only = message.automatic
            && encoding.truncated
            && encoding.bytes.len() == control_capacity
            && fitting.len() == 1
            && fitting
                .first()
                .is_some_and(|control| matches!(control, ControlMessage::Credentials { .. }));
        let staged = ReceiveControl {
            descriptors: numbers,
            truncated: encoding.truncated && !automatic_only,
            controls,
        };
        copyout(&message.payload, &staged)?;
        let descriptors = transaction.commit().map_err(|_| ControlError::Invalid)?;
        Ok(ReceiveControl {
            descriptors,
            truncated: staged.truncated,
            controls: staged.controls,
        })
    }

    fn effective_controls(
        queued: &[ControlMessage],
        credentials: Option<SenderCredentials>,
        automatic: bool,
    ) -> Vec<ControlMessage> {
        if !automatic {
            return queued.to_vec();
        }
        let mut controls = Vec::with_capacity(queued.len() + usize::from(credentials.is_some()));
        if let Some(credentials) = credentials {
            controls.push(ControlMessage::Credentials {
                process: credentials.process,
                user: credentials.user,
                group: credentials.group,
            });
        }
        controls.extend_from_slice(queued);
        controls
    }

    fn received_controls(queued: &[ControlMessage], descriptors: &[i32]) -> Vec<ControlMessage> {
        let mut offset = 0;
        queued
            .iter()
            .filter_map(|control| match control {
                ControlMessage::Rights(sent) => {
                    let end = (offset + sent.len()).min(descriptors.len());
                    let received = descriptors[offset..end].to_vec();
                    offset = end;
                    (!received.is_empty()).then_some(ControlMessage::Rights(received))
                }
                other => Some(other.clone()),
            })
            .collect()
    }
}

impl Default for UnixMessageQueue {
    fn default() -> Self {
        Self::new()
    }
}
