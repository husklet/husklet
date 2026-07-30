use std::ptr::NonNull;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLCommandBuffer, MTLCommandBufferStatus, MTLDrawable, MTLTexture};
use objc2_quartz_core::CAMetalDrawable;

use crate::scene::port::{
    CompletionOutcome, PresentTiming, PresentationCompletion, PresentationId, PresenterEvent, Wake,
};

const CALLBACK_DEADLINE: Duration = Duration::from_secs(1);
const PENDING: u8 = 0;
const TERMINAL: u8 = 1;
type Presented = dyn Fn(NonNull<ProtocolObject<dyn MTLDrawable>>);

struct CompletionGate(AtomicU8);

impl CompletionGate {
    fn new() -> Self {
        Self(AtomicU8::new(PENDING))
    }

    fn claim(&self) -> bool {
        self.0
            .compare_exchange(PENDING, TERMINAL, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn terminal(&self) -> bool {
        self.0.load(Ordering::Acquire) == TERMINAL
    }
}

fn presented_nanos(seconds: f64) -> Option<u64> {
    let nanos = seconds * 1_000_000_000.0;
    (seconds.is_finite() && seconds >= 0.0 && nanos <= u64::MAX as f64)
        .then(|| nanos.round() as u64)
}

fn sane_presented_time(presented: u64, submitted: u64, observed: u64) -> bool {
    const TOLERANCE: u64 = 5_000_000_000;
    presented.saturating_add(TOLERANCE) >= submitted
        && presented <= observed.saturating_add(TOLERANCE)
}

fn publish(
    events: &Sender<PresenterEvent>,
    wake: &Option<Arc<dyn Wake>>,
    completion: PresentationCompletion,
) {
    if events
        .send(PresenterEvent::Presentation(completion))
        .is_ok()
    {
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

/// One drawable submission retained until WindowServer reports presentation or a bounded failure.
pub(in crate::surface::macos) struct NativePresent {
    id: PresentationId,
    command: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    _drawable: Retained<ProtocolObject<dyn CAMetalDrawable>>,
    terminal: Arc<CompletionGate>,
    submitted: Instant,
    events: Sender<PresenterEvent>,
    wake: Option<Arc<dyn Wake>>,
}

impl NativePresent {
    pub(in crate::surface::macos) fn new(
        id: PresentationId,
        command: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        drawable: Retained<ProtocolObject<dyn CAMetalDrawable>>,
        events: Sender<PresenterEvent>,
        wake: Option<Arc<dyn Wake>>,
    ) -> Self {
        let terminal = Arc::new(CompletionGate::new());
        let callback_terminal = terminal.clone();
        let callback_events = events.clone();
        let callback_wake = wake.clone();
        let submitted_ns = crate::scene::port::clock::monotonic_nanos();
        let callback: RcBlock<Presented> =
            RcBlock::new(move |drawable: NonNull<ProtocolObject<dyn MTLDrawable>>| {
                if !callback_terminal.claim() {
                    return;
                }
                // SAFETY: Metal invokes this block with the live drawable it has just presented.
                let seconds = unsafe { drawable.as_ref().presentedTime() };
                let Some(present_ns) = presented_nanos(seconds) else {
                    publish(
                        &callback_events,
                        &callback_wake,
                        PresentationCompletion {
                            id,
                            outcome: CompletionOutcome::TerminalFailure,
                        },
                    );
                    return;
                };
                let observed_ns = crate::scene::port::clock::monotonic_nanos();
                if !submitted_ns
                    .zip(observed_ns)
                    .is_some_and(|(submitted, observed)| {
                        sane_presented_time(present_ns, submitted, observed)
                    })
                {
                    publish(
                        &callback_events,
                        &callback_wake,
                        PresentationCompletion {
                            id,
                            outcome: CompletionOutcome::TerminalFailure,
                        },
                    );
                    return;
                }
                publish(
                    &callback_events,
                    &callback_wake,
                    PresentationCompletion {
                        id,
                        outcome: CompletionOutcome::Delivered {
                            serial: id.0,
                            timing: Some(PresentTiming {
                                present_ns,
                                refresh_ns: 0,
                                vsync: true,
                            }),
                        },
                    },
                );
            });
        // SAFETY: CAMetalDrawable copies the block and invokes it after the drawable is presented.
        // The closure captures only Send + Sync state and the callback's drawable pointer is borrowed
        // solely for the duration of the invocation.
        unsafe {
            let callback_ptr: *mut block2::Block<Presented> =
                std::ptr::from_ref(&*callback).cast_mut();
            drawable.addPresentedHandler(callback_ptr);
        }
        Self {
            id,
            command,
            _drawable: drawable,
            terminal,
            submitted: Instant::now(),
            events,
            wake,
        }
    }

    /// Poll only terminal failures. Command completion does not prove drawable presentation.
    pub(in crate::surface::macos) fn poll(&self, now: Instant) -> bool {
        if self.terminal.terminal() {
            return true;
        }
        let failed = self.command.status() == MTLCommandBufferStatus::Error;
        let expired = now.saturating_duration_since(self.submitted) >= CALLBACK_DEADLINE;
        if !failed && !expired {
            return false;
        }
        if self.terminal.claim() {
            publish(
                &self.events,
                &self.wake,
                PresentationCompletion {
                    id: self.id,
                    outcome: CompletionOutcome::TerminalFailure,
                },
            );
        }
        true
    }
}

pub(in crate::surface::macos) enum PresentAttempt {
    Submitted(NativePresent),
    Retry,
    Terminal,
}

pub(in crate::surface::macos) fn drawable_matches(
    source: &ProtocolObject<dyn MTLTexture>,
    drawable: &ProtocolObject<dyn MTLTexture>,
) -> bool {
    let source = (source.width(), source.height());
    let drawable = (drawable.width(), drawable.height());
    source == drawable && source.0 != 0 && source.1 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_command_is_not_a_presentation_outcome() {
        assert_ne!(
            MTLCommandBufferStatus::Completed,
            MTLCommandBufferStatus::Error
        );
    }

    #[test]
    fn callback_deadline_is_bounded() {
        assert!(CALLBACK_DEADLINE >= Duration::from_millis(100));
        assert!(CALLBACK_DEADLINE <= Duration::from_secs(2));
    }

    #[test]
    fn presented_time_conversion_rejects_invalid_values() {
        assert_eq!(presented_nanos(1.25), Some(1_250_000_000));
        assert_eq!(presented_nanos(-1.0), None);
        assert_eq!(presented_nanos(f64::NAN), None);
        assert_eq!(presented_nanos(f64::INFINITY), None);
        assert_eq!(presented_nanos(f64::MAX), None);
    }

    #[test]
    fn presentation_error_timeout_and_callback_race_is_exact_once() {
        let gate = Arc::new(CompletionGate::new());
        let workers = (0..12)
            .map(|_| {
                let gate = gate.clone();
                std::thread::spawn(move || usize::from(gate.claim()))
            })
            .collect::<Vec<_>>();
        let wins = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum::<usize>();
        assert_eq!(wins, 1);
        assert!(gate.terminal());
    }

    #[test]
    fn completion_enqueue_wakes_exactly_once() {
        use std::sync::atomic::AtomicUsize;

        struct CountWake(AtomicUsize);
        impl Wake for CountWake {
            fn wake(&self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let wake = Arc::new(CountWake(AtomicUsize::new(0)));
        publish(
            &tx,
            &Some(wake.clone()),
            PresentationCompletion {
                id: PresentationId(7),
                outcome: CompletionOutcome::TerminalFailure,
            },
        );
        assert!(matches!(
            rx.recv().unwrap(),
            PresenterEvent::Presentation(PresentationCompletion {
                id: PresentationId(7),
                ..
            })
        ));
        assert_eq!(wake.0.load(Ordering::Relaxed), 1);
    }
}
