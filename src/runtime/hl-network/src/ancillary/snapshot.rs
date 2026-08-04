use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use hl_descriptor::DescriptionRef;
use hl_sync::WaitQueue;

use super::queue::{
    ControlError, ControlMessage, QUEUE_DATA_MAXIMUM, QUEUE_MESSAGE_MAXIMUM, QueueMessageSnapshot, QueueRightsSnapshot,
    QueueSnapshot, QueuedRights, RIGHTS_MAXIMUM, UnixMessage, UnixMessageQueue,
};

impl UnixMessageQueue {
    #[must_use]
    pub fn snapshot(&self) -> QueueSnapshot {
        let messages = self.messages.lock().unwrap_or_else(|error| error.into_inner());
        QueueSnapshot {
            messages: messages.iter().map(Self::snapshot_message).collect(),
        }
    }

    fn snapshot_message(message: &UnixMessage) -> QueueMessageSnapshot {
        QueueMessageSnapshot {
            payload: message.payload.clone(),
            controls: message
                .controls
                .iter()
                .map(|control| match control {
                    ControlMessage::Rights(_) => ControlMessage::Rights(Vec::new()),
                    other => other.clone(),
                })
                .collect(),
            rights: message
                .rights
                .iter()
                .map(|rights| QueueRightsSnapshot {
                    identities: rights.rights.iter().map(DescriptionRef::identity).collect(),
                })
                .collect(),
            credentials: message.credentials,
            automatic: message.automatic,
        }
    }

    pub fn restore<F>(snapshot: &QueueSnapshot, mut rebind: F) -> Result<Self, ControlError>
    where
        F: FnMut(u64) -> Option<DescriptionRef>,
    {
        snapshot.validate()?;
        let mut messages = VecDeque::new();
        for saved in &snapshot.messages {
            let rights = saved
                .rights
                .iter()
                .map(|saved_rights| Self::restore_rights(saved_rights, &mut rebind))
                .collect::<Result<Vec<_>, _>>()?;
            messages.push_back(UnixMessage {
                payload: saved.payload.clone(),
                controls: Self::restore_controls(saved)?,
                rights,
                credentials: saved.credentials,
                automatic: saved.automatic,
            });
        }
        Ok(Self {
            messages: Mutex::new(messages),
            wait: Arc::new(WaitQueue::new()),
            passcred: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn restore_controls(saved: &QueueMessageSnapshot) -> Result<Vec<ControlMessage>, ControlError> {
        let mut rights = saved.rights.iter();
        let mut controls = Vec::with_capacity(saved.controls.len());
        for control in &saved.controls {
            match control {
                ControlMessage::Rights(_) => {
                    let group = rights.next().ok_or(ControlError::Invalid)?;
                    controls.push(ControlMessage::Rights(vec![0; group.identities.len()]));
                }
                other => controls.push(other.clone()),
            }
        }
        Ok(controls)
    }

    fn restore_rights<F>(saved: &QueueRightsSnapshot, rebind: &mut F) -> Result<QueuedRights, ControlError>
    where
        F: FnMut(u64) -> Option<DescriptionRef>,
    {
        let rights = saved
            .identities
            .iter()
            .map(|identity| rebind(*identity).ok_or(ControlError::MissingDescription))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueuedRights { rights })
    }
}

impl QueueSnapshot {
    pub fn validate(&self) -> Result<(), ControlError> {
        if self.messages.len() > QUEUE_MESSAGE_MAXIMUM {
            return Err(ControlError::TooBig);
        }
        for message in &self.messages {
            if message.payload.len() > QUEUE_DATA_MAXIMUM || Self::rights_total(message).is_none() {
                return Err(ControlError::TooBig);
            }
            if Self::invalid_shape(message) {
                return Err(ControlError::Invalid);
            }
            if Self::invalid_credentials(message) {
                return Err(ControlError::PermissionDenied);
            }
        }
        Ok(())
    }

    fn invalid_shape(message: &QueueMessageSnapshot) -> bool {
        let records = message
            .controls
            .iter()
            .filter(|control| matches!(control, ControlMessage::Rights(_)))
            .count();
        records != message.rights.len() || message.rights.iter().any(|entry| entry.identities.contains(&0))
    }

    fn invalid_credentials(message: &QueueMessageSnapshot) -> bool {
        let mut explicit = message.controls.iter().filter_map(|control| {
            let ControlMessage::Credentials { process, user, group } = control else {
                return None;
            };
            Some(super::SenderCredentials {
                process: *process,
                user: *user,
                group: *group,
            })
        });
        let invalid_automatic = message.automatic
            && (message.credentials.is_none()
                || message
                    .controls
                    .iter()
                    .any(|control| matches!(control, ControlMessage::Credentials { .. })));
        invalid_automatic || explicit.any(|credentials| Some(credentials) != message.credentials)
    }

    fn rights_total(message: &QueueMessageSnapshot) -> Option<usize> {
        message.rights.iter().try_fold(0_usize, |total, entry| {
            total
                .checked_add(entry.identities.len())
                .filter(|count| *count <= RIGHTS_MAXIMUM)
        })
    }
}
