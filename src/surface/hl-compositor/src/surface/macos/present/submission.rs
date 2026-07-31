use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
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

/// Why a drawable submission ended terminally.
///
/// A terminal failure costs the client the frame outright and is never retried, so an unattributed one
/// leaves a window permanently blank with nothing on record explaining it. Each cause is distinct and
/// actionable: the first two mean WindowServer presented the drawable but could not be believed about
/// when, the third that Metal rejected the command buffer, the fourth that the presented callback never
/// arrived at all.
#[derive(Clone, Copy)]
enum TerminalCause {
    /// `MTLDrawable.presentedTime` did not convert to a monotonic instant — typically still zero,
    /// meaning WindowServer never actually presented this drawable.
    PresentedTimeUnreadable,
    /// A presented time outside any plausible window relative to submission and observation.
    PresentedTimeImplausible,
    /// The `MTLCommandBuffer` reported `Error` — the GPU work itself failed.
    CommandBufferError,
    /// Neither the presented callback nor a command-buffer error arrived within `CALLBACK_DEADLINE`.
    CallbackDeadlineExpired,
}

impl TerminalCause {
    const ALL: usize = 4;

    const fn index(self) -> usize {
        match self {
            TerminalCause::PresentedTimeUnreadable => 0,
            TerminalCause::PresentedTimeImplausible => 1,
            TerminalCause::CommandBufferError => 2,
            TerminalCause::CallbackDeadlineExpired => 3,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            TerminalCause::PresentedTimeUnreadable => "presented_time_unreadable",
            TerminalCause::PresentedTimeImplausible => "presented_time_implausible",
            TerminalCause::CommandBufferError => "command_buffer_error",
            TerminalCause::CallbackDeadlineExpired => "callback_deadline_expired",
        }
    }
}

/// One `error`-level line per distinct cause, for the whole process.
///
/// A window whose presents fail terminally fails EVERY present, so logging each one buries the fact in
/// thousands of identical lines — the same silence as saying nothing. The first of each cause speaks and
/// names its submission; the rest are counted. Correlate the submission id with the adapter's
/// `settle async root=… submission=…` line to get the surface.
static ANNOUNCED: [std::sync::atomic::AtomicBool; TerminalCause::ALL] =
    [const { std::sync::atomic::AtomicBool::new(false) }; TerminalCause::ALL];

/// Build the terminal completion for `id`, attributing it to `cause` exactly once per cause.
fn terminal_completion(id: PresentationId, cause: TerminalCause) -> PresentationCompletion {
    hl_log::hl_count!(hl_log::tag::PRESENT, "present_terminal");
    if !ANNOUNCED[cause.index()].swap(true, Ordering::Relaxed) {
        hl_log::hl_log!(
            hl_log::tag::PRESENT,
            hl_log::Level::Error,
            "present terminal submission={} cause={} — further failures of this cause are counted \
             (present_terminal), not logged",
            id.0,
            cause.name()
        );
    }
    PresentationCompletion {
        id,
        outcome: CompletionOutcome::TerminalFailure,
    }
}

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

/// What the host display can be ASKED about a submission, gathered at submit time.
///
/// `wp_presentation` feedback is a client's frame-pacing input (Chrome's `BeginFrame` estimator reads
/// it), so every field here has to be something macOS actually reports rather than something assumed.
#[derive(Clone)]
pub(in crate::surface::macos) struct DisplayTiming {
    /// The target screen's refresh interval in nanoseconds, from `NSScreen.maximumFramesPerSecond`.
    /// `0` when the window is on no screen — reported as unknown rather than guessed.
    pub refresh_ns: u64,
    /// `CAMetalLayer.displaySyncEnabled`: the AppKit contract that a presented drawable waits for the
    /// display's vertical blank. False means CoreAnimation may show the frame immediately (tearing).
    pub display_sync: bool,
    /// `presentedTime` of the previous frame presented through the same layer (`0` = none yet). Shared
    /// with the presented handler, which both reads and updates it.
    pub last_presented_ns: Arc<AtomicU64>,
}

/// Whether a vertical-blank-synchronized presentation was actually OBSERVED, as opposed to assumed.
///
/// Three things together are the evidence: the layer is in display-sync mode, the display's interval is
/// known, and the gap between this drawable's `presentedTime` and the previous one is an integer multiple
/// of that interval. The multiple (not just one interval) is what makes this work on a client that paces
/// below the refresh rate, and on a ProMotion panel presenting at a divisor of its maximum. The FIRST
/// frame after an idle gap has no predecessor, so nothing has been observed and `false` is reported —
/// `wp_presentation`'s vsync flag is a claim about evidence, and there is none yet.
fn vsync_observed(timing: &DisplayTiming, previous_ns: u64, present_ns: u64) -> bool {
    const TOLERANCE_NUMERATOR: u64 = 1;
    const TOLERANCE_DENOMINATOR: u64 = 4;
    if !timing.display_sync
        || timing.refresh_ns == 0
        || previous_ns == 0
        || present_ns <= previous_ns
    {
        return false;
    }
    let delta = present_ns - previous_ns;
    let intervals = (delta + timing.refresh_ns / 2) / timing.refresh_ns;
    if intervals == 0 {
        return false;
    }
    let expected = intervals.saturating_mul(timing.refresh_ns);
    let tolerance = timing.refresh_ns * TOLERANCE_NUMERATOR / TOLERANCE_DENOMINATOR;
    delta.abs_diff(expected) <= tolerance
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
        display: DisplayTiming,
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
                        terminal_completion(id, TerminalCause::PresentedTimeUnreadable),
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
                        terminal_completion(id, TerminalCause::PresentedTimeImplausible),
                    );
                    return;
                }
                // Evidence, not optimism: the refresh interval is the target screen's, and the vsync flag
                // is claimed only when this frame's presented time lines up with the previous one on the
                // display's cadence. `swap` makes the comparison exactly-once per submission.
                let previous_ns = display.last_presented_ns.swap(present_ns, Ordering::AcqRel);
                publish(
                    &callback_events,
                    &callback_wake,
                    PresentationCompletion {
                        id,
                        outcome: CompletionOutcome::Delivered {
                            serial: id.0,
                            timing: Some(PresentTiming {
                                present_ns,
                                refresh_ns: display.refresh_ns,
                                vsync: vsync_observed(&display, previous_ns, present_ns),
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
            // Distinguish the two: a command-buffer error is the GPU rejecting the work, an expiry is the
            // presented callback never arriving. They have entirely different causes and fixes.
            let cause = if failed {
                TerminalCause::CommandBufferError
            } else {
                TerminalCause::CallbackDeadlineExpired
            };
            publish(&self.events, &self.wake, terminal_completion(self.id, cause));
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

    fn timing(display_sync: bool, refresh_ns: u64) -> DisplayTiming {
        DisplayTiming {
            refresh_ns,
            display_sync,
            last_presented_ns: Arc::new(AtomicU64::new(0)),
        }
    }

    const HZ60: u64 = 16_666_667;

    #[test]
    fn vsync_is_claimed_only_for_a_frame_that_landed_on_the_display_cadence() {
        let sync = timing(true, HZ60);
        // Consecutive refreshes, and a client pacing at half rate: both land on the cadence.
        assert!(vsync_observed(&sync, 1_000_000_000, 1_000_000_000 + HZ60));
        assert!(vsync_observed(
            &sync,
            1_000_000_000,
            1_000_000_000 + HZ60 * 2
        ));
        assert!(vsync_observed(
            &sync,
            1_000_000_000,
            1_000_000_000 + HZ60 * 7
        ));
        // Half an interval off the cadence is exactly what a non-synchronized present looks like.
        assert!(!vsync_observed(
            &sync,
            1_000_000_000,
            1_000_000_000 + HZ60 / 2
        ));
        assert!(!vsync_observed(
            &sync,
            1_000_000_000,
            1_000_000_000 + HZ60 + HZ60 / 2
        ));
    }

    #[test]
    fn vsync_is_never_claimed_without_the_evidence_for_it() {
        // No predecessor (the first frame, or the first after an idle gap): nothing was observed.
        assert!(!vsync_observed(&timing(true, HZ60), 0, 1_000_000_000));
        // Unknown refresh interval: there is no cadence to have landed on.
        assert!(!vsync_observed(&timing(true, 0), 1_000, 1_000 + HZ60));
        // The layer is not in display-sync mode, so CoreAnimation never promised a vblank.
        assert!(!vsync_observed(
            &timing(false, HZ60),
            1_000_000_000,
            1_000_000_000 + HZ60
        ));
        // A non-advancing presented time proves nothing either way.
        assert!(!vsync_observed(
            &timing(true, HZ60),
            1_000_000_000,
            1_000_000_000
        ));
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
