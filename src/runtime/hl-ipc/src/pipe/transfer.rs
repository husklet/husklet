use std::sync::Arc;

use hl_descriptor::{ObjectError, OperationCancellation, PipeTransferEndpoint};

use crate::pipe::{EndpointDirection, PIPE_BUF, PipeCancellationWake, PipeEndpoint, PipeState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferMode {
    Move,
    Duplicate,
}
pub type PipeTransferMode = TransferMode;

enum TransferAttempt {
    Complete(usize),
    WaitSource,
    WaitTarget,
    Error(ObjectError),
}

pub struct Transfer;
pub type PipeTransfer = Transfer;

impl PipeTransfer {
    pub fn execute(
        source: &dyn PipeTransferEndpoint,
        target: &dyn PipeTransferEndpoint,
        maximum: usize,
        mode: PipeTransferMode,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        let source = source
            .as_any()
            .downcast_ref::<PipeEndpoint>()
            .ok_or(ObjectError::InvalidArgument)?;
        let target = target
            .as_any()
            .downcast_ref::<PipeEndpoint>()
            .ok_or(ObjectError::InvalidArgument)?;
        Self::validate(source, target)?;
        if maximum == 0 {
            return Ok(0);
        }
        let _source_subscription = Self::subscribe(source, cancellation);
        let _target_subscription = Self::subscribe(target, cancellation);
        loop {
            match Self::attempt(source, target, maximum, mode) {
                TransferAttempt::Complete(count) => {
                    return Ok(Self::complete(source, target, count));
                }
                TransferAttempt::Error(error) => return Err(error),
                TransferAttempt::WaitSource if nonblocking => {
                    return Err(ObjectError::WouldBlock);
                }
                TransferAttempt::WaitTarget if nonblocking => {
                    return Err(ObjectError::WouldBlock);
                }
                TransferAttempt::WaitSource => {
                    drop(source.wait_for_readable(cancellation)?);
                }
                TransferAttempt::WaitTarget => {
                    drop(target.wait_write_space(1, cancellation)?);
                }
            }
        }
    }

    fn complete(source: &PipeEndpoint, target: &PipeEndpoint, count: usize) -> usize {
        if count != 0 {
            let source_state = source
                .pipe
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            source.pipe.notify_sleepers(&source_state);
            drop(source_state);
            let target_state = target
                .pipe
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            target.pipe.notify_sleepers(&target_state);
            drop(target_state);
            source.notify_readiness();
            target.notify_readiness();
        }
        count
    }

    fn validate(source: &PipeEndpoint, target: &PipeEndpoint) -> Result<(), ObjectError> {
        if source.direction != EndpointDirection::Read
            || target.direction != EndpointDirection::Write
            || Arc::ptr_eq(&source.pipe, &target.pipe)
        {
            return Err(ObjectError::InvalidArgument);
        }
        Ok(())
    }

    fn subscribe(
        endpoint: &PipeEndpoint,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Option<Box<dyn hl_descriptor::CancellationSubscription>> {
        cancellation.map(|cancellation| {
            cancellation.subscribe(Arc::new(PipeCancellationWake {
                pipe: Arc::downgrade(&endpoint.pipe),
            }))
        })
    }

    fn attempt(
        source: &PipeEndpoint,
        target: &PipeEndpoint,
        maximum: usize,
        mode: PipeTransferMode,
    ) -> TransferAttempt {
        let source_key = Arc::as_ptr(&source.pipe) as usize;
        let target_key = Arc::as_ptr(&target.pipe) as usize;
        if source_key < target_key {
            let mut source_state = source
                .pipe
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut target_state = target
                .pipe
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::locked(&mut source_state, &mut target_state, maximum, mode)
        } else {
            let mut target_state = target
                .pipe
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut source_state = source
                .pipe
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::locked(&mut source_state, &mut target_state, maximum, mode)
        }
    }

    fn locked(
        source: &mut PipeState,
        target: &mut PipeState,
        maximum: usize,
        mode: PipeTransferMode,
    ) -> TransferAttempt {
        if source.splice_reserved {
            return TransferAttempt::WaitSource;
        }
        if source.bytes.is_empty() {
            return if source.writers == 0 {
                TransferAttempt::Complete(0)
            } else if source.read_nonblocking {
                TransferAttempt::Error(ObjectError::WouldBlock)
            } else {
                TransferAttempt::WaitSource
            };
        }
        if target.readers == 0 {
            return TransferAttempt::Error(ObjectError::BrokenPipe);
        }
        let space = target.capacity - target.bytes.len();
        if space == 0 {
            return if target.write_nonblocking {
                TransferAttempt::Error(ObjectError::WouldBlock)
            } else {
                TransferAttempt::WaitTarget
            };
        }
        let source_record = source.packets.front().copied().unwrap_or(source.bytes.len());
        let mut count = maximum.min(source.bytes.len()).min(space);
        if source.packet_mode || target.packet_mode {
            count = count.min(source_record).min(PIPE_BUF);
        }
        target.bytes.extend(source.bytes.iter().take(count).copied());
        if target.packet_mode {
            target.packets.push_back(count);
        }
        if mode == PipeTransferMode::Move {
            Self::consume(source, source_record, count);
        }
        TransferAttempt::Complete(count)
    }

    fn consume(source: &mut PipeState, source_record: usize, count: usize) {
        source.bytes.drain(..count);
        if !source.packet_mode {
            return;
        }
        if count == source_record {
            source.packets.pop_front();
        } else if let Some(record) = source.packets.front_mut() {
            *record -= count;
        }
    }
}
