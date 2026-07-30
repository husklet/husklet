//! Bounded ownership transfer for native compositor frames.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};

static NEXT_INGRESS_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeFrameOutcome {
    Displayed,
    Discarded,
    Replaced,
    StaleToken,
    Duplicate,
    Decreasing,
    Capacity,
    ImportFailed,
    TerminalFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeFrameCompletion {
    pub token: NonZeroU64,
    pub serial: NonZeroU64,
    pub outcome: NativeFrameOutcome,
}

pub struct NativeFrame {
    pub(crate) token: NonZeroU64,
    pub(crate) serial: NonZeroU64,
    pub(crate) surface: hl_iosurface::Surface,
    readiness: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    readiness_group: Option<u64>,
    completion: Option<SyncSender<NativeFrameCompletion>>,
}

impl fmt::Debug for NativeFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFrame")
            .field("token", &self.token)
            .field("serial", &self.serial)
            .field("iosurface", &self.surface.id())
            .finish_non_exhaustive()
    }
}

impl NativeFrame {
    pub fn new(
        token: u64,
        serial: u64,
        surface: hl_iosurface::Surface,
    ) -> Result<Self, NativeFrameError> {
        Ok(Self {
            token: NonZeroU64::new(token).ok_or(NativeFrameError::ZeroToken)?,
            serial: NonZeroU64::new(serial).ok_or(NativeFrameError::ZeroSerial)?,
            surface,
            readiness: None,
            readiness_group: None,
            completion: None,
        })
    }

    /// Attach a nonblocking producer-completion probe. The compositor retains the frame and polls this
    /// before importing the IOSurface, so GPU submission never blocks its producer and the allocation
    /// remains alive until both GPU production and host presentation have finished.
    pub fn with_readiness(mut self, readiness: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.readiness_group = Some(0);
        self.readiness = Some(Arc::new(readiness));
        self
    }

    /// Attach readiness to one producer device. Frames remain FIFO within a device while an unrelated
    /// device can make progress if this device's oldest frame is still pending.
    pub fn with_device_readiness(
        mut self,
        device: u64,
        readiness: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.readiness_group = Some(device);
        self.readiness = Some(Arc::new(readiness));
        self
    }

    pub(crate) fn complete(mut self, outcome: NativeFrameOutcome) {
        self.send(outcome);
    }

    fn send(&mut self, outcome: NativeFrameOutcome) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let _ = completion.try_send(NativeFrameCompletion {
            token: self.token,
            serial: self.serial,
            outcome,
        });
    }
}

impl Drop for NativeFrame {
    fn drop(&mut self) {
        self.send(NativeFrameOutcome::Discarded);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeFrameError {
    ZeroToken,
    ZeroSerial,
    ZeroCapacity,
    Closed,
}

#[derive(Debug)]
pub struct NativeFrameReceipt(Receiver<NativeFrameCompletion>);

impl NativeFrameReceipt {
    pub fn try_complete(&self) -> Result<Option<NativeFrameCompletion>, TryRecvError> {
        match self.0.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub struct NativeFrameSender(Arc<Mutex<Ingress>>, SyncSender<NativeFrameCancellation>);

pub struct NativeFrames {
    ingress: Arc<Mutex<Ingress>>,
    cancellations: Receiver<NativeFrameCancellation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeFrameCancellation {
    pub(crate) token: NonZeroU64,
    pub(crate) serial: NonZeroU64,
}

struct Ingress {
    id: u64,
    frames: VecDeque<NativeFrame>,
    serial_capacity: usize,
    token_capacity: usize,
    senders: usize,
    receiver: bool,
}

#[derive(Debug)]
pub struct NativeFramePublishError {
    pub frame: NativeFrame,
    pub reason: NativeFramePublishFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeFramePublishFailure {
    Closed,
}

pub fn native_frames(
    capacity: usize,
) -> Result<(NativeFrameSender, NativeFrames), NativeFrameError> {
    if capacity == 0 {
        return Err(NativeFrameError::ZeroCapacity);
    }
    // Cancellation is lossless and bounded. A producer briefly backpressures when the compositor falls
    // behind rather than dropping an exact key and leaving its already-committed Wayland frame unresolved.
    let cancellation_capacity = capacity.saturating_mul(128).max(256);
    let (cancel_sender, cancel_receiver) = mpsc::sync_channel(cancellation_capacity);
    let ingress = Arc::new(Mutex::new(Ingress {
        id: NEXT_INGRESS_ID.fetch_add(1, Ordering::Relaxed),
        frames: VecDeque::with_capacity(capacity),
        serial_capacity: capacity,
        token_capacity: cancellation_capacity,
        senders: 1,
        receiver: true,
    }));
    Ok((
        NativeFrameSender(Arc::clone(&ingress), cancel_sender),
        NativeFrames {
            ingress,
            cancellations: cancel_receiver,
        },
    ))
}

impl Clone for NativeFrameSender {
    fn clone(&self) -> Self {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .senders += 1;
        Self(Arc::clone(&self.0), self.1.clone())
    }
}

impl Drop for NativeFrameSender {
    fn drop(&mut self) {
        let mut ingress = self.0.lock().unwrap_or_else(|error| error.into_inner());
        ingress.senders = ingress.senders.saturating_sub(1);
    }
}

impl Drop for NativeFrames {
    fn drop(&mut self) {
        let mut ingress = self
            .ingress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        ingress.receiver = false;
        for frame in ingress.frames.drain(..) {
            frame.complete(NativeFrameOutcome::TerminalFailure);
        }
    }
}

impl NativeFrameSender {
    /// Terminally cancel one reserved native-frame identity after its GPU submission failed.
    ///
    /// The producer may have exposed this pair through a shared buffer before execution began, allowing a
    /// Wayland commit to wait for it. Cancellation travels through the same native ingress as successful
    /// frames so the compositor can settle that exact commit without retiring unrelated frames or surfaces.
    pub fn cancel(&self, token: u64, serial: u64) -> Result<(), NativeFrameError> {
        let cancellation = NativeFrameCancellation {
            token: NonZeroU64::new(token).ok_or(NativeFrameError::ZeroToken)?,
            serial: NonZeroU64::new(serial).ok_or(NativeFrameError::ZeroSerial)?,
        };
        self.1
            .send(cancellation)
            .map_err(|_| NativeFrameError::Closed)
    }

    pub fn publish(
        &self,
        mut frame: NativeFrame,
    ) -> Result<NativeFrameReceipt, NativeFramePublishError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        frame.completion = Some(sender);
        let mut ingress = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if !ingress.receiver {
            return Err(NativeFramePublishError {
                frame,
                reason: NativeFramePublishFailure::Closed,
            });
        }

        let newest = ingress
            .frames
            .iter()
            .filter(|queued| queued.token == frame.token)
            .map(|queued| queued.serial)
            .max();
        if let Some(newest) = newest {
            if frame.serial <= newest {
                let outcome = if frame.serial == newest {
                    NativeFrameOutcome::Duplicate
                } else {
                    NativeFrameOutcome::Decreasing
                };
                frame.complete(outcome);
                return Ok(NativeFrameReceipt(receiver));
            }
        }
        let serials = ingress
            .frames
            .iter()
            .filter(|queued| queued.token == frame.token)
            .count();
        if serials == ingress.serial_capacity {
            if let Some(index) = ingress
                .frames
                .iter()
                .position(|queued| queued.token == frame.token)
            {
                let evicted = ingress.frames.remove(index).expect("index came from queue");
                evicted.complete(NativeFrameOutcome::Capacity);
            }
        }
        let tokens = ingress
            .frames
            .iter()
            .map(|queued| queued.token)
            .collect::<std::collections::HashSet<_>>();
        if !tokens.contains(&frame.token) && tokens.len() == ingress.token_capacity {
            if let Some(evicted) = ingress.frames.pop_front() {
                evicted.complete(NativeFrameOutcome::Capacity);
            }
        }
        ingress.frames.push_back(frame);
        hl_log::hl_log!(
            hl_log::tag::COMPOSITOR,
            hl_log::Level::Debug,
            "native ingress published id={} pending={}",
            ingress.id,
            ingress.frames.len()
        );
        Ok(NativeFrameReceipt(receiver))
    }
}

impl NativeFrames {
    pub(crate) fn try_cancel(&self) -> Result<Option<NativeFrameCancellation>, TryRecvError> {
        match self.cancellations.try_recv() {
            Ok(cancellation) => Ok(Some(cancellation)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn try_next(&self) -> Result<Option<NativeFrame>, TryRecvError> {
        let candidates = {
            let ingress = self
                .ingress
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut groups = std::collections::HashSet::new();
            ingress
                .frames
                .iter()
                .filter(|frame| {
                    frame
                        .readiness_group
                        .is_none_or(|group| groups.insert(group))
                })
                .map(|frame| {
                    (
                        frame.token,
                        frame.serial,
                        frame.readiness.as_ref().map(Arc::clone),
                    )
                })
                .collect::<Vec<_>>()
        };
        let ready = candidates
            .into_iter()
            .find(|(_, _, readiness)| readiness.as_ref().is_none_or(|probe| probe()));
        let Some((token, serial, _)) = ready else {
            let ingress = self
                .ingress
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            return if ingress.frames.is_empty() && ingress.senders == 0 {
                Err(TryRecvError::Disconnected)
            } else {
                Ok(None)
            };
        };
        let mut ingress = self
            .ingress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = ingress
            .frames
            .iter()
            .position(|frame| frame.token == token && frame.serial == serial)
        {
            let frame = ingress.frames.remove(index).expect("index came from queue");
            hl_log::hl_log!(
                hl_log::tag::COMPOSITOR,
                hl_log::Level::Debug,
                "native ingress consumed id={} pending={}",
                ingress.id,
                ingress.frames.len()
            );
            Ok(Some(frame))
        } else if ingress.senders == 0 {
            Err(TryRecvError::Disconnected)
        } else {
            Ok(None)
        }
    }

    /// Remove the oldest queued frame for `token` that precedes `serial`.
    ///
    /// Frames and failed-submission cancellations use separate bounded ingress paths. Before applying a
    /// cancellation, the compositor drains earlier successful frames for that same token so the exact
    /// cancellation cannot consume an older unversioned Wayland commit.
    pub(crate) fn try_before(&self, token: NonZeroU64, serial: NonZeroU64) -> Option<NativeFrame> {
        let candidate = {
            let ingress = self
                .ingress
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ingress
                .frames
                .iter()
                .find(|frame| frame.token == token && frame.serial < serial)
                .map(|frame| (frame.serial, frame.readiness.as_ref().map(Arc::clone)))
        }?;
        if candidate.1.as_ref().is_some_and(|probe| !probe()) {
            return None;
        }
        let mut ingress = self
            .ingress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let index = ingress
            .frames
            .iter()
            .position(|frame| frame.token == token && frame.serial == candidate.0)?;
        let frame = ingress.frames.remove(index)?;
        hl_log::hl_log!(
            hl_log::tag::COMPOSITOR,
            hl_log::Level::Debug,
            "native ingress consumed-before-cancel id={} pending={}",
            ingress.id,
            ingress.frames.len()
        );
        Some(frame)
    }

    pub(crate) fn capacity(&self) -> usize {
        self.ingress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .serial_capacity
    }

    pub(crate) fn id(&self) -> u64 {
        self.ingress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .id
    }
}

#[cfg(test)]
include!("receipt.rs");
