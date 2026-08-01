//! The guest Vulkan driver's logging composition root, plus the latch its per-frame sites use.
//!
//! `hl-log` has TWO independent gates and this ICD satisfied neither. Compile-time, a release build
//! keeps `hl_error!` and strips `warn`/`info`/`debug`/`trace` — and until now every diagnostic in this
//! driver was `warn` or below, so the shipped `libvk_hl.so.1` could not report a failure at all.
//! Runtime, [`hl_log::Logging::enabled`] tests a process-wide tag mask that starts at
//! `AtomicU64::new(0)` and is only ever opened by an explicit [`hl_log::Config::apply`], which
//! `hl-log` assigns to "the application composition root". The guest driver never had one.
//!
//! Promoting the failures to `hl_error!` without this module would buy nothing: the level would pass
//! the compile-time check and then die on a mask nobody opens. This is that missing composition root,
//! the mirror of `hl-gl`'s (commit 91971d9).
//!
//! Off by default. A Vulkan ICD loaded into Chrome's GPU process must not write to stderr unless it
//! was asked to, so an absent `HL_VK_LOG` leaves the mask exactly where it is today: closed.

use hl_log::{tag, Config, Level, Tags};
use std::sync::{Mutex, Once};

/// Tag list to open, e.g. `HL_VK_LOG=present,vulkan` or `HL_VK_LOG=all`. Unset or `off` stays quiet.
/// Any non-empty request also opens `transport` and `wire`: the guest→host transport is on this
/// driver's failure path, and nobody diagnosing a lost device should have to know that.
const TAGS_VARIABLE: &str = "HL_VK_LOG";
/// Maximum severity to emit, e.g. `HL_VK_LOG_LEVEL=warn`. Defaults to `error`, the level a release
/// build still compiles in — asking for more in a release build is accepted and simply has less to
/// report, because the verbose macros are already gone.
const LEVEL_VARIABLE: &str = "HL_VK_LOG_LEVEL";
/// The general variable the host forwards into the guest. Used when the specific one above is unset, so
/// one `HL_LOG` reaches host and guest alike instead of four names a person has to know.
const GENERAL_TAGS_VARIABLE: &str = "HL_LOG";
/// The general level variable, used when the specific one is unset.
const GENERAL_LEVEL_VARIABLE: &str = "HL_LOG_LEVEL";

pub struct GuestLogging;

impl GuestLogging {
    /// Open the runtime gate from the environment, once per shared object.
    ///
    /// Called from [`crate::state::StateStore::with`], the single funnel every `vk*` entry point takes
    /// before it can touch state or report anything, so the gate is open before the first diagnostic.
    /// A single relaxed atomic on every call after the first. Unlike `hl-gl`, this driver ships as one
    /// object (`libvk_hl.so.1`), so one funnel is one mask.
    pub(crate) fn install() {
        static ONCE: Once = Once::new();
        ONCE.call_once(Self::apply_from_environment);
    }

    fn apply_from_environment() {
        // The specific variable wins, and `HL_LOG` is the fallback when it is unset.
        //
        // Without the fallback this gate had no key through the product's own launch path. The host
        // forwards only `HL_LOG`, `HL_LOG_LEVEL` and `HL_LOG_COUNTERS` across the sanitized worker into
        // the guest (`apps/husklet/src/bin/host/environment.rs`), and `HL_VK_LOG` is not among them —
        // so setting it on the host did nothing, and the guest never saw a tag. Every guest-side
        // diagnostic in the product was therefore unreachable in any build at any level, which is why
        // nobody had ever seen one. The specific variable keeps working for harnesses that inject the
        // guest environment directly, where it is still the precise control.
        let tags = std::env::var(TAGS_VARIABLE)
            .ok()
            .or_else(|| std::env::var(GENERAL_TAGS_VARIABLE).ok());
        let level = std::env::var(LEVEL_VARIABLE)
            .ok()
            .or_else(|| std::env::var(GENERAL_LEVEL_VARIABLE).ok());
        Self::configure(tags.as_deref(), level.as_deref());
    }

    /// Open the gate for an explicit request, or leave it closed. Split from the environment read so
    /// the decision is testable without mutating process environment under a parallel test runner.
    pub(crate) fn configure(tags: Option<&str>, level: Option<&str>) {
        let Some(requested) = tags else {
            return;
        };
        // `Tags: FromStr` is infallible and maps `off` / `none` / unknown names to NONE.
        let logging: Tags = requested.parse().unwrap_or(Tags::NONE);
        if logging == Tags::NONE {
            return;
        }
        // The guest→host transport is part of THIS driver's failure path, not a separate subsystem the
        // caller opted out of. Its sites are tagged TRANSPORT/WIRE, so a mask parsed from the
        // documented `HL_VK_LOG=vulkan` left "host executor rejected frame" — the one line that
        // explains any DEVICE_LOST — masked for anyone following the documented usage. Unioned only
        // once the caller has asked for something, so absent/`off` still leaves the gate closed.
        let logging = logging | Tags::from(tag::TRANSPORT) | Tags::from(tag::WIRE);
        let level = level.and_then(Level::from_name).unwrap_or(Level::Error);
        Config {
            logging,
            level,
            // Profiling stays closed: counters and timings are a separate opt-in with their own cost.
            profiling: Tags::NONE,
        }
        .apply();
    }
}

/// A one-shot gate keyed by an object handle, so a failure on a PER-FRAME path reports once per
/// swapchain instead of once per frame.
///
/// `vkQueuePresentKHR` and `vkAcquireNextImageKHR` run at display rate, and the failures worth an
/// `error` here are exactly the ones that persist — a surface that cannot be committed to fails on
/// every frame until the app recreates the swapchain. An unlatched `error!` on that path would emit
/// sixty identical lines a second and bury the one that mattered, which is the same uselessness as
/// emitting none. Keyed rather than global so a second swapchain still gets to speak.
pub(crate) struct Latch(Mutex<Vec<u64>>);

impl Latch {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    /// `true` the first time `key` is seen, `false` forever after. Keys are Vulkan handles, which the
    /// driver never recycles, so a recreated swapchain is a new key and reports again.
    pub(crate) fn fires(&self, key: u64) -> bool {
        let mut seen = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_log::sink::Sink;

    /// Lines the driver actually emitted, so a test asserts arrival at the sink rather than asserting
    /// by inspection that a call site exists.
    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The tag mask, the level and the sink are all process-global, so these tests must not interleave.
    static SERIAL: Mutex<()> = Mutex::new(());

    struct Collector;

    impl Sink for Collector {
        fn write_line(&self, line: &str) {
            LINES
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(line.to_owned());
        }
    }

    fn captured(needle: &str) -> bool {
        LINES
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|line| line.contains(needle))
    }

    fn start_capture() {
        LINES
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        hl_log::Output::global().set(Box::new(Collector));
    }

    fn end_capture() {
        hl_log::Logging::global().set(Tags::NONE);
        hl_log::Output::global().reset();
    }

    /// A driver `hl_error!` reaches the sink ONLY once the composition root has opened the runtime tag
    /// mask. The closed half is what the shipped ICD did on every call: the mask is `AtomicU64::new(0)`
    /// and nothing in the guest ever opened it.
    #[test]
    fn error_reaches_the_sink_only_after_the_gate_is_opened() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();

        hl_log::Logging::global().set(Tags::NONE);
        GuestLogging::configure(Some("off"), None);
        hl_log::hl_error!(hl_log::tag::PRESENT, "sentinel-while-closed");
        assert!(
            !captured("sentinel-while-closed"),
            "a closed mask must discard the line"
        );

        GuestLogging::configure(Some("present"), Some("error"));
        hl_log::hl_error!(hl_log::tag::PRESENT, "sentinel-after-open");
        assert!(
            captured("sentinel-after-open"),
            "HL_VK_LOG=present must let a present-tagged error reach the sink"
        );

        // Opting one tag in never turns the whole driver loud.
        hl_log::hl_error!(hl_log::tag::CUDA, "sentinel-unrequested-tag");
        assert!(
            !captured("sentinel-unrequested-tag"),
            "an unrequested tag must stay closed"
        );

        end_capture();
    }

    /// An absent variable must leave the gate exactly as it found it: an ICD loaded into Chrome's GPU
    /// process does not start writing to stderr merely because it was loaded.
    #[test]
    fn absent_or_empty_variable_leaves_the_gate_closed() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();
        hl_log::Logging::global().set(Tags::NONE);

        for request in [None, Some(""), Some("off"), Some("none"), Some("nonsense")] {
            GuestLogging::configure(request, None);
            hl_log::hl_error!(hl_log::tag::PRESENT, "sentinel-should-stay-quiet");
            assert!(
                !captured("sentinel-should-stay-quiet"),
                "{request:?} must leave the gate closed"
            );
        }

        end_capture();
    }

    /// The per-frame latch speaks once per swapchain, not once per frame, and a different swapchain is
    /// still allowed to speak.
    #[test]
    fn latch_fires_once_per_key() {
        let latch = Latch::new();
        assert!(latch.fires(0x10), "the first failure on a swapchain reports");
        for _ in 0..60 {
            assert!(!latch.fires(0x10), "a repeat on the same swapchain is mute");
        }
        assert!(latch.fires(0x20), "a second swapchain still gets to speak");
    }

    /// The transport is reachable from the DOCUMENTED usage. Its sites are tagged `TRANSPORT`/`WIRE`,
    /// not `tag::VULKAN`, so before the union `HL_VK_LOG=vulkan` left "host executor rejected frame" —
    /// the single line that explains any `DEVICE_LOST` — masked for anyone following the documentation.
    #[test]
    fn opening_the_driver_also_opens_the_transport_it_fails_through() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();
        hl_log::Logging::global().set(Tags::NONE);

        GuestLogging::configure(Some("vulkan"), Some("error"));
        hl_log::hl_error!(hl_log::tag::TRANSPORT, "sentinel-transport");
        hl_log::hl_error!(hl_log::tag::WIRE, "sentinel-wire");
        assert!(
            captured("sentinel-transport") && captured("sentinel-wire"),
            "HL_VK_LOG=vulkan must reach the transport that carries this driver's work"
        );

        end_capture();
    }

    /// The union must not turn an unasked-for driver loud: it adds the transport, nothing else.
    #[test]
    fn the_transport_union_does_not_open_unrequested_subsystems() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();
        hl_log::Logging::global().set(Tags::NONE);

        GuestLogging::configure(Some("vulkan"), Some("error"));
        hl_log::hl_error!(hl_log::tag::COMPOSITOR, "sentinel-compositor");
        assert!(
            !captured("sentinel-compositor"),
            "the union adds the transport only"
        );

        // And an absent or refused request still opens nothing at all, transport included.
        hl_log::Logging::global().set(Tags::NONE);
        for request in [None, Some(""), Some("off"), Some("none")] {
            GuestLogging::configure(request, None);
            hl_log::hl_error!(hl_log::tag::TRANSPORT, "sentinel-closed-transport");
            assert!(
                !captured("sentinel-closed-transport"),
                "{request:?} must leave even the transport closed"
            );
        }

        end_capture();
    }

    /// The body the end-to-end test below runs in a FRESH process. It does NOT call `install` itself —
    /// it calls a real exported `vk*` entry point and then reports, so what is under test is that
    /// ENTRY POINTS open the gate, not merely that `install` works when invoked by hand.
    /// `vkDestroySwapchainKHR` is the cheapest one that funnels through `StateStore::with` and opens no
    /// socket. Ignored by default: only meaningful with `HL_VK_LOG` set and a virgin `Once`.
    #[test]
    #[ignore = "driven as a subprocess by install_reads_the_environment_end_to_end"]
    fn emit_sentinel_after_install() {
        crate::vkDestroySwapchainKHR(std::ptr::null_mut(), 0, std::ptr::null());
        hl_log::hl_error!(hl_log::tag::PRESENT, "SUBPROCESS-SENTINEL");
    }

    /// End to end, in a real process, through the real stderr sink: setting `HL_VK_LOG` makes a driver
    /// `hl_error!` arrive, and NOT setting it makes the same call site silent. Asserting this in-process
    /// is impossible — `install` latches a `Once` and the mask is global — so the check re-executes this
    /// test binary, which is also the only way to exercise the environment read itself.
    #[test]
    fn install_reads_the_environment_end_to_end() {
        let run = |value: Option<&str>| {
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("test binary path"));
            command.args([
                "--exact",
                "--ignored",
                "--nocapture",
                "logging::tests::emit_sentinel_after_install",
            ]);
            match value {
                Some(value) => command.env(TAGS_VARIABLE, value),
                None => command.env_remove(TAGS_VARIABLE),
            };
            let output = command.output().expect("re-exec the test binary");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "the subprocess body must have run"
            );
            String::from_utf8_lossy(&output.stderr).into_owned()
        };

        assert!(
            run(Some("present")).contains("SUBPROCESS-SENTINEL"),
            "HL_VK_LOG=present must make the driver's error reach stderr"
        );
        assert!(
            !run(None).contains("SUBPROCESS-SENTINEL"),
            "with no HL_VK_LOG the driver must stay silent, exactly as it does today"
        );
    }
}
