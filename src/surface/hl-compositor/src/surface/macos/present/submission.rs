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

use crate::scene::model::SurfaceId;
use crate::scene::port::{
    CompletionOutcome, PresentTiming, PresentationCompletion, PresentationId, PresenterEvent, Wake,
};

const CALLBACK_DEADLINE: Duration = Duration::from_secs(1);
const PENDING: u8 = 0;
const TERMINAL: u8 = 1;
type Presented = dyn Fn(NonNull<ProtocolObject<dyn MTLDrawable>>);

/// Why a drawable submission failed.
///
/// A failed present costs the client the frame, so an unattributed one leaves a window blank with nothing
/// on record explaining it. Each cause is distinct and actionable: the first means WindowServer never
/// displayed the drawable at all, the second that it claims a presentation time nothing else agrees with,
/// the third that Metal rejected the command buffer, the fourth that the presented callback never arrived.
#[derive(Clone, Copy)]
enum FailureCause {
    /// `MTLDrawable.presentedTime` did not name an instant — zero, the value Metal leaves for a drawable
    /// WindowServer never displayed. The frame did not reach the screen; nothing says it cannot next time.
    PresentedTimeUnreadable,
    /// A presented time outside any plausible window relative to submission and observation.
    PresentedTimeImplausible,
    /// The `MTLCommandBuffer` reported `Error` — the GPU work itself failed.
    CommandBufferError,
    /// Neither the presented callback nor a command-buffer error arrived within `CALLBACK_DEADLINE`.
    CallbackDeadlineExpired,
}

impl FailureCause {
    const fn index(self) -> usize {
        match self {
            FailureCause::PresentedTimeUnreadable => 0,
            FailureCause::PresentedTimeImplausible => 1,
            FailureCause::CommandBufferError => 2,
            FailureCause::CallbackDeadlineExpired => 3,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            FailureCause::PresentedTimeUnreadable => "presented_time_unreadable",
            FailureCause::PresentedTimeImplausible => "presented_time_implausible",
            FailureCause::CommandBufferError => "command_buffer_error",
            FailureCause::CallbackDeadlineExpired => "callback_deadline_expired",
        }
    }

    /// What this cause proves about the NEXT present, given how many times it has already fired on this
    /// surface.
    ///
    /// Only a cause that says the drawable can never be believed again is terminal. A drawable that was
    /// simply not displayed is the offscreen case the pacing model already carries: retain the frame and
    /// its callbacks and re-drive it, rather than dropping the client's callbacks outright — a window that
    /// is off screen now may be on screen at the next refresh, and a client whose callbacks were dropped
    /// never asks again.
    ///
    /// Three of the four causes are statements about the drawable and answer without reference to the
    /// count: an unreadable presented time means it was not displayed and says nothing about the next
    /// one; an implausible time means the drawable's own accounting cannot be trusted; a command-buffer
    /// error means the work did not happen. `CallbackDeadlineExpired` is the exception, and the reason it
    /// needs `seen`: it is a statement about ELAPSED TIME rather than about the drawable — the only one of
    /// the four whose firing depends on how busy the machine is. A single expiry is what every observed
    /// recovery looked like; a surface that keeps missing the deadline is the window WindowServer never
    /// composites, and retrying that forever leaves a client waiting on frames that will not come.
    fn outcome(self, seen: u32) -> CompletionOutcome {
        match self {
            FailureCause::PresentedTimeUnreadable => CompletionOutcome::RetryableFailure,
            FailureCause::PresentedTimeImplausible | FailureCause::CommandBufferError => {
                CompletionOutcome::TerminalFailure
            }
            // The bound is ONE, read off the logs rather than chosen round. Every occurrence that
            // recovered was a single expiry during window creation — `chrome-arm` at +3287ms and
            // +4601ms, a windowed GLES client at +1136ms, all at count=1 — and every pathological one
            // climbed immediately, reaching count=10 within nine seconds on the same surface. Nothing
            // observed sat between the two, so the first repeat is the earliest point that separates
            // them and it separates them with margin.
            FailureCause::CallbackDeadlineExpired if seen <= 1 => CompletionOutcome::RetryableFailure,
            FailureCause::CallbackDeadlineExpired => CompletionOutcome::TerminalFailure,
        }
    }
}

/// Occurrence counts per surface per cause, reported on the [`milestone`](crate::diagnostic::milestone)
/// schedule.
///
/// Per SURFACE, not per process: a process-global latch answers "does this cause ever fire" — worth
/// asking exactly once, after which the first failing client hides every other one failing the same way.
/// And COUNTED, not latched: `presented_time_unreadable` firing once as a window is created is a
/// transient the window recovers from, while the same cause firing every frame is a window that is
/// never composited. A single line looks identical either way, and reading one as the other cost this
/// project a build cycle and two misrouted investigations.
///
/// Completions arrive on Metal's callback threads, so this is shared state rather than presenter state.
static ANNOUNCED: std::sync::Mutex<Option<crate::diagnostic::Tally<(SurfaceId, usize)>>> =
    std::sync::Mutex::new(None);

/// The running count for `(surface, cause)` when this occurrence should speak. A poisoned lock reports
/// rather than goes silent — over-reporting a failure is recoverable, losing it is what this path is for.
fn claim_announcement(surface: SurfaceId, cause: FailureCause) -> Option<crate::diagnostic::Occurrence> {
    let Ok(mut announced) = ANNOUNCED.lock() else {
        return Some(crate::diagnostic::Occurrence {
            count: 0,
            since: std::time::Duration::ZERO,
        });
    };
    announced
        .get_or_insert_with(crate::diagnostic::Tally::new)
        .record((surface, cause.index()))
}

/// How many times each `(surface, cause)` has fired. Separate from [`ANNOUNCED`], which is a REPORTING
/// schedule that deliberately speaks only at milestones — a decision cannot be taken from a counter that
/// skips occurrences. Completions arrive on Metal's callback threads, so this is shared state.
static OCCURRENCES: std::sync::Mutex<Option<std::collections::HashMap<(SurfaceId, usize), u32>>> =
    std::sync::Mutex::new(None);

/// The number of times `cause` has now fired on `surface`, this occurrence included. A poisoned lock
/// answers 1, the recoverable direction: one more retry costs a frame, while a spurious terminal costs
/// the client every frame after it.
fn occurrences(surface: SurfaceId, cause: FailureCause) -> u32 {
    let Ok(mut counts) = OCCURRENCES.lock() else {
        return 1;
    };
    let counts = counts.get_or_insert_with(std::collections::HashMap::new);
    let seen = counts.entry((surface, cause.index())).or_insert(0);
    *seen = seen.saturating_add(1);
    *seen
}

/// Build the failure completion for `id`, attributing it to `cause` once per surface.
fn failed_completion(
    id: PresentationId,
    surface: SurfaceId,
    cause: FailureCause,
) -> PresentationCompletion {
    let outcome = cause.outcome(occurrences(surface, cause));
    let retryable = matches!(outcome, CompletionOutcome::RetryableFailure);
    if retryable {
        hl_log::hl_count!(hl_log::tag::PRESENT, "present_retry");
    } else {
        hl_log::hl_count!(hl_log::tag::PRESENT, "present_terminal");
    }
    if let Some(seen) = claim_announcement(surface, cause) {
        // Report the SPAN as well as the count. A count alone cannot distinguish 100 failures in 100
        // frames (a dead window) from 100 over six minutes (an intermittent one under 1%), and stating
        // only the count led a reader straight to the first conclusion when the data said the second.
        // The reading advice differs by OUTCOME, and giving the retryable one to a terminal cause is
        // actively misleading. For a retryable failure the rate is the question: a count tracking the
        // frame rate is a window that never composites, a count far below it is intermittent and the
        // frame is re-driven. For a TERMINAL failure the rate is beside the point — the first
        // occurrence already retired that surface's frame and dropped its callbacks, so "only
        // intermittent" is not available as a conclusion. An earlier version of this line told the
        // reader a count of one was "a transient it recovers from" for a cause that recovers from
        // nothing; that text is gone, but inviting the same division for both outcomes leaves the same
        // wrong conclusion one step further away.
        hl_log::hl_log!(
            hl_log::tag::PRESENT,
            hl_log::Level::Error,
            "present {} surface={} submission={} cause={} count={} over_ms={} — {}",
            if retryable { "retryable" } else { "terminal" },
            surface.0,
            id.0,
            cause.name(),
            seen.count,
            seen.since.as_millis(),
            if retryable {
                "divide before concluding: a count that tracks the frame rate is a window that never \
                 composites, a count far below it is intermittent and the frame is re-driven"
            } else {
                "TERMINAL: this surface's frame was retired and its callbacks dropped. The count is the \
                 number of frames lost, not a rate to be excused — the first one already cost a frame"
            }
        );
    }
    PresentationCompletion { id, outcome }
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

/// Darwin's `CLOCK_UPTIME_RAW`: nanoseconds since boot EXCLUDING time asleep — the `mach_absolute_time`
/// epoch that `CACurrentMediaTime`, and therefore `MTLDrawable.presentedTime`, is stamped on.
///
/// Deliberately not [`crate::scene::port::clock::monotonic_nanos`], for the same reason
/// `present/latency.rs` reads its own clock for `NSEvent.timestamp`: that reads `CLOCK_MONOTONIC`, which
/// on Darwin INCLUDES time asleep. Measured on the host this was found on, the two epochs stood
/// 13257.094632 s apart while `CLOCK_UPTIME_RAW` matched `CACurrentMediaTime` to 0.000000 — so comparing
/// a drawable's presented time against `CLOCK_MONOTONIC` put every real presentation 3.7 hours "before"
/// its own submission, far outside any tolerance. Every present was judged implausible and terminated,
/// from submission 1, with no recovery: no present, so no frame callback, so no client ever drew again.
fn drawable_now_nanos() -> Option<u64> {
    #[repr(C)]
    struct Timespec {
        seconds: i64,
        nanos: i64,
    }
    extern "C" {
        fn clock_gettime(clock: i32, value: *mut Timespec) -> i32;
    }
    const CLOCK_UPTIME_RAW: i32 = 8;

    let mut value = Timespec {
        seconds: 0,
        nanos: 0,
    };
    // SAFETY: `value` is a valid writable timespec and CLOCK_UPTIME_RAW is supported on Darwin.
    let result = unsafe { clock_gettime(CLOCK_UPTIME_RAW, &mut value) };
    (result == 0 && value.seconds >= 0 && (0..1_000_000_000).contains(&value.nanos)).then(|| {
        (value.seconds as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(value.nanos as u64)
    })
}

/// Move an instant from the drawable's epoch into `CLOCK_MONOTONIC`, the clock id this compositor
/// advertises on `wp_presentation` (`state/lifecycle.rs`).
///
/// A client is told which clock its presentation timestamps are on and has every right to subtract them
/// from its own readings of it. Publishing a `mach_absolute_time` value into that domain hands Chrome's
/// `BeginFrame` estimator a timestamp hours in the past. Both clocks are read together so the offset is
/// the sleep time, not the cost of this function.
fn monotonic_domain_nanos(drawable_ns: u64) -> Option<u64> {
    let drawable_now = drawable_now_nanos()?;
    let monotonic_now = crate::scene::port::clock::monotonic_nanos()?;
    Some(drawable_ns.saturating_add(monotonic_now.saturating_sub(drawable_now)))
}

/// `MTLDrawable.presentedTime` as an instant on the drawable's own clock, or `None` when it does not name
/// one.
///
/// Zero is the value Metal leaves in place for a drawable WindowServer never actually displayed, and it
/// is the single most common reading on a window that is not on screen — so it has to be rejected HERE.
/// Admitting it as `Some(0)` sent it on to `sane_presented_time` as if it were a real presentation.
fn presented_nanos(seconds: f64) -> Option<u64> {
    let nanos = seconds * 1_000_000_000.0;
    (seconds.is_finite() && seconds > 0.0 && nanos <= u64::MAX as f64)
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

/// Whether a reported presentation time sits plausibly between when the frame was submitted and when the
/// callback was observed. All three MUST be readings of the drawable's own clock ([`drawable_now_nanos`]);
/// mixing epochs here makes the check reject every real presentation rather than the implausible ones.
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

/// Who a drawable submission belongs to and where its completion is reported.
///
/// Grouped rather than passed as four positional arguments: they travel together everywhere, and the
/// surface exists purely so a failure can name the window it cost.
pub(in crate::surface::macos) struct Submission {
    pub id: PresentationId,
    pub surface: SurfaceId,
    pub events: Sender<PresenterEvent>,
    pub wake: Option<Arc<dyn Wake>>,
}

/// The one question [`NativePresent::poll`] asks the command buffer. Behind a trait solely so a test can
/// construct a submission without a `CAMetalDrawable` — which needs a `CAMetalLayer`, which needs a
/// window, which needs the main thread the cargo harness does not have. That chain is why no test drove
/// this path before: the code was unreachable from the harness rather than overlooked.
///
/// Injection only. The real construction site passes the real command buffer and the runtime behaviour is
/// the status read it always was. No `Send` bound: a `NativePresent` already holds `Retained` Metal
/// objects and is thread-affine, so requiring it here only fails to compile against the real type.
pub(in crate::surface::macos) trait CommandStatus {
    fn failed(&self) -> bool;
}

impl CommandStatus for Retained<ProtocolObject<dyn MTLCommandBuffer>> {
    fn failed(&self) -> bool {
        self.status() == MTLCommandBufferStatus::Error
    }
}

/// One drawable submission retained until WindowServer reports presentation or a bounded failure.
pub(in crate::surface::macos) struct NativePresent {
    id: PresentationId,
    surface: SurfaceId,
    command: Box<dyn CommandStatus>,
    /// Held only to keep the drawable alive for the submission's lifetime. `None` only in a test that
    /// drives the deadline, which has no drawable to hold.
    _drawable: Option<Retained<ProtocolObject<dyn CAMetalDrawable>>>,
    terminal: Arc<CompletionGate>,
    submitted: Instant,
    events: Sender<PresenterEvent>,
    wake: Option<Arc<dyn Wake>>,
}

impl NativePresent {
    pub(in crate::surface::macos) fn new(
        submission: Submission,
        command: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
        drawable: Retained<ProtocolObject<dyn CAMetalDrawable>>,
        display: DisplayTiming,
    ) -> Self {
        let Submission {
            id,
            surface,
            events,
            wake,
        } = submission;
        let terminal = Arc::new(CompletionGate::new());
        let callback_terminal = terminal.clone();
        let callback_events = events.clone();
        let callback_wake = wake.clone();
        // The drawable's epoch, NOT `monotonic_nanos` — this is compared against `presentedTime`.
        let submitted_ns = drawable_now_nanos();
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
                        failed_completion(id, surface, FailureCause::PresentedTimeUnreadable),
                    );
                    return;
                };
                let observed_ns = drawable_now_nanos();
                if !submitted_ns
                    .zip(observed_ns)
                    .is_some_and(|(submitted, observed)| {
                        sane_presented_time(present_ns, submitted, observed)
                    })
                {
                    publish(
                        &callback_events,
                        &callback_wake,
                        failed_completion(id, surface, FailureCause::PresentedTimeImplausible),
                    );
                    return;
                }
                // Evidence, not optimism: the refresh interval is the target screen's, and the vsync flag
                // is claimed only when this frame's presented time lines up with the previous one on the
                // display's cadence. `swap` makes the comparison exactly-once per submission.
                let previous_ns = display.last_presented_ns.swap(present_ns, Ordering::AcqRel);
                let vsync = vsync_observed(&display, previous_ns, present_ns);
                // Cadence above is compared drawable-clock to drawable-clock. What LEAVES here is a client's
                // `wp_presentation` timestamp, so it is stated on the clock this compositor advertised; an
                // instant we cannot place on that clock is reported as absent rather than on the wrong one.
                let timing = monotonic_domain_nanos(present_ns).map(|present_ns| PresentTiming {
                    present_ns,
                    refresh_ns: display.refresh_ns,
                    vsync,
                });
                publish(
                    &callback_events,
                    &callback_wake,
                    PresentationCompletion {
                        id,
                        outcome: CompletionOutcome::Delivered {
                            serial: id.0,
                            timing,
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
            surface,
            command: Box::new(command),
            _drawable: Some(drawable),
            terminal,
            submitted: Instant::now(),
            events,
            wake,
        }
    }

    /// A submission with no drawable, for driving [`Self::poll`]'s failure classification in a test.
    ///
    /// Every field the poll path reads is real: the same gate, the same `submitted` instant, the same
    /// channel. Only the drawable — which the poll path never touches, holding it solely to keep it
    /// alive — is absent, and only the command status is substitutable. `submitted` is taken as an
    /// argument so a deadline can be expired without a test sleeping for a second.
    #[cfg(test)]
    pub(in crate::surface::macos) fn for_test(
        id: PresentationId,
        surface: SurfaceId,
        command: Box<dyn CommandStatus>,
        submitted: Instant,
        events: Sender<PresenterEvent>,
    ) -> Self {
        Self {
            id,
            surface,
            command,
            _drawable: None,
            terminal: Arc::new(CompletionGate::new()),
            submitted,
            events,
            wake: None,
        }
    }

    /// Poll only terminal failures. Command completion does not prove drawable presentation.
    pub(in crate::surface::macos) fn poll(&self, now: Instant) -> bool {
        if self.terminal.terminal() {
            return true;
        }
        let failed = self.command.failed();
        let expired = now.saturating_duration_since(self.submitted) >= CALLBACK_DEADLINE;
        if !failed && !expired {
            return false;
        }
        if self.terminal.claim() {
            // Distinguish the two: a command-buffer error is the GPU rejecting the work, an expiry is the
            // presented callback never arriving. They have entirely different causes and fixes.
            let cause = if failed {
                FailureCause::CommandBufferError
            } else {
                FailureCause::CallbackDeadlineExpired
            };
            publish(&self.events, &self.wake, failed_completion(self.id, self.surface, cause));
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
    fn a_drawable_window_server_never_displayed_is_unreadable_and_retryable() {
        // Metal leaves `presentedTime` at zero for a drawable that never reached the screen. Admitting it
        // as an instant sent it to `sane_presented_time`, which compares against machine-uptime nanos and
        // failed it as implausible — the wrong cause, and terminal, which drops the client's frame
        // callbacks so the window never advances again.
        assert_eq!(presented_nanos(0.0), None);
        assert!(presented_nanos(1.0).is_some());
        let uptime_ns = 12 * 60 * 60 * 1_000_000_000u64;
        assert!(!sane_presented_time(0, uptime_ns, uptime_ns));
        assert_eq!(
            FailureCause::PresentedTimeUnreadable.outcome(1),
            CompletionOutcome::RetryableFailure
        );
        // These two are terminal on their FIRST occurrence, because each is a statement about the
        // drawable rather than about elapsed time. The deadline is deliberately absent from this list:
        // it is bounded-retryable and has its own tests.
        for terminal in [
            FailureCause::PresentedTimeImplausible,
            FailureCause::CommandBufferError,
        ] {
            assert_eq!(terminal.outcome(1), CompletionOutcome::TerminalFailure);
        }
        assert_eq!(
            FailureCause::CallbackDeadlineExpired.outcome(1),
            CompletionOutcome::RetryableFailure,
            "one expiry keeps the client's callbacks"
        );
        assert_eq!(
            FailureCause::CallbackDeadlineExpired.outcome(2),
            CompletionOutcome::TerminalFailure,
            "a repeat on the same surface is the window that never composites"
        );
    }

    #[test]
    fn the_drawable_clock_matches_the_drawable_epoch_and_monotonic_does_not() {
        // The bug this pins: `presentedTime` is on `mach_absolute_time`, which excludes time asleep, while
        // `monotonic_nanos` reads Darwin's `CLOCK_MONOTONIC`, which includes it. Comparing across the two
        // put every real presentation "before" its own submission by the machine's total sleep time and
        // terminated it. Measured 13257.094632 s apart on the host this was found on.
        let drawable = drawable_now_nanos().expect("CLOCK_UPTIME_RAW readable");
        let monotonic =
            crate::scene::port::clock::monotonic_nanos().expect("CLOCK_MONOTONIC readable");
        assert!(
            monotonic >= drawable,
            "CLOCK_MONOTONIC includes sleep, so it can never trail the drawable clock"
        );

        // A frame presented one refresh after submission is sane on one clock and rejected across two.
        let refresh = 16_666_667;
        let submitted = drawable;
        let presented = submitted + refresh;
        let observed = presented + 200_000;
        assert!(sane_presented_time(presented, submitted, observed));
        let slept = 13_257_094_632_000u64;
        assert!(
            !sane_presented_time(presented, submitted + slept, observed + slept),
            "this is exactly the misclassification: a real presentation judged implausible"
        );
    }

    #[test]
    fn a_published_presentation_time_is_stated_on_the_advertised_clock() {
        // `wp_presentation` is advertised as CLOCK_MONOTONIC (state/lifecycle.rs), so what a client
        // receives has to be comparable with its own reading of that clock, not with the drawable's.
        let drawable = drawable_now_nanos().expect("CLOCK_UPTIME_RAW readable");
        let published = monotonic_domain_nanos(drawable).expect("both clocks readable");
        let monotonic =
            crate::scene::port::clock::monotonic_nanos().expect("CLOCK_MONOTONIC readable");
        assert!(
            published.abs_diff(monotonic) < 1_000_000_000,
            "a just-now drawable instant must publish as a just-now monotonic instant \
             (published={published} monotonic={monotonic})"
        );
    }

    struct Command(bool);
    impl CommandStatus for Command {
        fn failed(&self) -> bool {
            self.0
        }
    }

    /// Drive a REAL deadline expiry through `poll` and read what it publishes.
    ///
    /// Returns the completion the surface's client would receive, or `None` if the poll reported nothing.
    fn expire(surface: u32, command_failed: bool) -> Option<CompletionOutcome> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let present = NativePresent::for_test(
            PresentationId(1),
            SurfaceId(surface),
            Box::new(Command(command_failed)),
            // Submitted a full deadline ago, so this poll is past it. Real elapsed time, no sleeping.
            Instant::now() - CALLBACK_DEADLINE,
            sender,
        );
        assert!(present.poll(Instant::now()), "an expired submission retires");
        receiver.try_iter().find_map(|event| match event {
            PresenterEvent::Presentation(completion) => Some(completion.outcome),
            _ => None,
        })
    }

    /// A client that waits for `wl_surface.frame` before drawing again must survive one expired deadline.
    ///
    /// This is the assertion that matters, and it is not about repainting: `reap_native_presents` clears
    /// the live submission and requests a repaint for a terminal failure exactly as for a retryable one,
    /// so the SURFACE recovers either way. What a terminal outcome costs is downstream — the frame's
    /// callbacks are dropped rather than retained (`scene::Scene::complete_presentation`), so a client
    /// waiting on one never asks for another frame and its window stops advancing. Measured: a windowed
    /// GLES client was killed by a single `callback_deadline_expired` at count=1, over_ms=0.
    ///
    /// A deadline expiring is a statement about elapsed time, not about the drawable — the only member of
    /// the terminal set that does not describe the thing being presented, and the only one whose firing
    /// depends on how busy the machine is. One expiry is the transient every observed occurrence at window
    /// creation turned out to be.
    #[test]
    fn one_expired_deadline_keeps_the_clients_callbacks() {
        assert_eq!(
            expire(1, false),
            Some(CompletionOutcome::RetryableFailure),
            "the first deadline expiry on a surface must retain the client's frame callbacks"
        );
    }

    /// The bound. A surface that keeps missing the deadline is the window WindowServer never composites,
    /// and retrying it forever means a client that never learns its frames are not arriving.
    ///
    /// The bound is ONE, taken from the logs rather than chosen round. Every occurrence that recovered was
    /// a single expiry during window creation (`chrome-arm` at +3287ms and +4601ms, `gl-steady` at
    /// +1136ms, all count=1); every pathological one climbed immediately, reaching count=10 within nine
    /// seconds on the same surface. Nothing observed sat between the two, so the first repeat is the
    /// earliest point that separates them and it separates them with margin.
    #[test]
    fn a_repeated_deadline_on_one_surface_is_terminal() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let outcomes: Vec<_> = (0..2)
            .map(|_| {
                let present = NativePresent::for_test(
                    PresentationId(1),
                    SurfaceId(7),
                    Box::new(Command(false)),
                    Instant::now() - CALLBACK_DEADLINE,
                    sender.clone(),
                );
                present.poll(Instant::now());
                receiver.try_iter().find_map(|event| match event {
                    PresenterEvent::Presentation(completion) => Some(completion.outcome),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            outcomes,
            vec![
                Some(CompletionOutcome::RetryableFailure),
                Some(CompletionOutcome::TerminalFailure)
            ],
            "the first expiry is retryable and the second on the same surface is terminal"
        );
    }

    /// The three causes that DO describe the drawable keep their classification. A harness able to produce
    /// only the outcome under investigation cannot show that the others still behave.
    #[test]
    fn a_command_buffer_error_is_still_terminal_on_the_first_occurrence() {
        assert_eq!(
            expire(2, true),
            Some(CompletionOutcome::TerminalFailure),
            "the GPU rejecting the work says the frame did not happen, whatever the count"
        );
        assert_eq!(
            FailureCause::PresentedTimeImplausible.outcome(1),
            CompletionOutcome::TerminalFailure
        );
        assert_eq!(
            FailureCause::PresentedTimeUnreadable.outcome(1),
            CompletionOutcome::RetryableFailure
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
