//! The guest driver's logging composition root.
//!
//! `hl-log` has TWO independent gates, and the guest driver only ever satisfied one of them.
//! Compile-time, a release build keeps `hl_error!` and strips everything below it. Runtime,
//! [`hl_log::Logging::enabled`] tests a process-wide tag mask that starts at `AtomicU64::new(0)` and is
//! only ever opened by an explicit [`hl_log::Config::apply`] — which `hl-log`'s own documentation assigns
//! to "the application composition root".
//!
//! The guest driver never had one. Nothing in the shim called `apply`, enabled a tag, or read an
//! environment variable, so the mask stayed 0 for the life of every guest process and **every
//! `hl_error!` in the driver was silently discarded, in debug and release alike** — including the ones
//! whose commit messages justified themselves on `error` being "the one level a release build keeps".
//! That is why so many of this driver's failures could only be found by interposing on it: it had no
//! voice. This module is that missing composition root.
//!
//! Off by default. A driver loaded into Chrome's GPU process must not write to stderr on a hot path
//! unless it was asked to, so an absent `HL_GL_LOG` leaves the mask exactly where it is today: closed.

use hl_log::{Config, Level, Tags};
use std::sync::Once;

/// Tag list to open, e.g. `HL_GL_LOG=present,egl` or `HL_GL_LOG=all`. Unset or `off` stays quiet.
const TAGS_VARIABLE: &str = "HL_GL_LOG";
/// Maximum severity to emit, e.g. `HL_GL_LOG_LEVEL=warn`. Defaults to `error`, the level a release build
/// still compiles in — asking for more than that in a release build is accepted and simply has less to
/// report, because the verbose macros are already gone.
const LEVEL_VARIABLE: &str = "HL_GL_LOG_LEVEL";
/// The general variable the host forwards into the guest. Used when the specific one above is unset, so
/// one `HL_LOG` reaches host and guest alike instead of four names a person has to know.
const GENERAL_TAGS_VARIABLE: &str = "HL_LOG";
/// The general level variable, used when the specific one is unset.
const GENERAL_LEVEL_VARIABLE: &str = "HL_LOG_LEVEL";

pub struct GuestLogging;

impl GuestLogging {
    /// Open the runtime gate from the environment, once per shared object.
    ///
    /// Called from [`crate::state::GlobalState::access`], so it runs before the first entry point can
    /// report anything, and is a single relaxed atomic on every call after the first. Deliberately
    /// per-object rather than per-process: `libEGL.so.1` and `libGLESv2.so.2` share one `State` but link
    /// their own copy of `hl-log`'s statics, so each has its own mask to open from the same variable.
    pub(crate) fn install() {
        static ONCE: Once = Once::new();
        ONCE.call_once(Self::apply_from_environment);
    }

    fn apply_from_environment() {
        // The specific variable wins, and `HL_LOG` is the fallback when it is unset.
        //
        // Without the fallback this gate had no key through the product's own launch path. The host
        // forwards only `HL_LOG`, `HL_LOG_LEVEL` and `HL_LOG_COUNTERS` across the sanitized worker into
        // the guest (`apps/husklet/src/bin/host/environment.rs`), and `HL_GL_LOG` is not among them —
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

    /// Open the gate for an explicit request, or leave it closed. Split from the environment read so the
    /// decision is testable without mutating process environment under a parallel test runner.
    pub(crate) fn configure(tags: Option<&str>, level: Option<&str>) {
        let Some(requested) = tags else {
            return;
        };
        // `Tags: FromStr` is infallible and maps `off` / `none` / unknown names to NONE.
        let logging: Tags = requested.parse().unwrap_or(Tags::NONE);
        if logging == Tags::NONE {
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use hl_log::sink::Sink;
    use std::sync::Mutex;

    /// Lines the driver actually emitted, so a test asserts arrival at the sink rather than asserting by
    /// inspection that a call site exists.
    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The tag mask, the level and the sink are all process-global, so these two tests must not interleave.
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
    /// mask, and the closed half of this test is what the shipped driver did on every call: the mask is
    /// `AtomicU64::new(0)` and nothing in the guest ever opened it, so every `error` the driver reported
    /// was discarded before it was even formatted — in debug and release alike.
    #[test]
    fn error_reaches_the_sink_only_after_the_gate_is_opened() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();

        // Closed — today's shipped behaviour.
        hl_log::Logging::global().set(Tags::NONE);
        GuestLogging::configure(Some("off"), None);
        hl_log::hl_error!(hl_log::tag::PRESENT, "sentinel-while-closed");
        assert!(
            !captured("sentinel-while-closed"),
            "a closed mask must discard the line"
        );

        // Opened for `present`, exactly as `HL_GL_LOG=present` does.
        GuestLogging::configure(Some("present"), Some("error"));
        hl_log::hl_error!(hl_log::tag::PRESENT, "sentinel-after-open");
        assert!(
            captured("sentinel-after-open"),
            "HL_GL_LOG=present must let a present-tagged error reach the sink"
        );

        // Opting one tag in never turns the whole driver loud.
        hl_log::hl_error!(hl_log::tag::CUDA, "sentinel-unrequested-tag");
        assert!(
            !captured("sentinel-unrequested-tag"),
            "an unrequested tag must stay closed"
        );

        end_capture();
    }

    /// An absent variable must leave the gate exactly as it found it: a driver loaded into Chrome's GPU
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

    /// The body the end-to-end test below runs in a FRESH process: install the composition root exactly
    /// as an entry point does, then report an error. Ignored by default because it is only meaningful
    /// with `HL_GL_LOG` set and a virgin `Once`.
    #[test]
    #[ignore = "driven as a subprocess by install_reads_the_environment_end_to_end"]
    fn emit_sentinel_after_install() {
        GuestLogging::install();
        hl_log::hl_error!(hl_log::tag::PRESENT, "SUBPROCESS-SENTINEL");
    }

    /// End to end, in a real process, through the real stderr sink: setting `HL_GL_LOG` makes a driver
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
            "HL_GL_LOG=present must make the driver's error reach stderr"
        );
        assert!(
            !run(None).contains("SUBPROCESS-SENTINEL"),
            "with no HL_GL_LOG the driver must stay silent, exactly as it does today"
        );
    }
}
