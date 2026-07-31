//! The guest CUDA driver's logging composition root.
//!
//! `hl-log` has TWO independent gates and the CUDA shims satisfied neither. Compile-time, a release
//! build keeps `hl_error!` and strips `warn`/`info`/`debug`/`trace` — and until now every diagnostic in
//! this crate was `warn` or below, so the shipped `libcuda.so.1` / `libcudart.so.1` could not report a
//! failure at all. Runtime, [`hl_log::Logging::enabled`] tests a process-wide tag mask that starts at
//! `AtomicU64::new(0)` and is only ever opened by an explicit [`hl_log::Config::apply`], which `hl-log`
//! assigns to "the application composition root". The guest driver never had one.
//!
//! Promoting the failures to `hl_error!` without this module would buy nothing: the level would pass
//! the compile-time check and then die on a mask nobody opens. This is that missing composition root,
//! the mirror of `hl-gl`'s (commit 91971d9).
//!
//! It lives in the shared lowering crate rather than in a shim because there are THREE guest objects
//! (`libcuda.so.1`, `libcudart.so.1`, `libnvidia-ml.so.1`), each of which links its own copy of both
//! this crate and `hl-log`'s statics — so each has its own mask to open, from the same variable, and
//! [`GuestLogging::install`]'s `Once` is per-object exactly as it needs to be. Each shim calls it from
//! its own state funnel.
//!
//! Off by default. A CUDA driver injected into an arbitrary guest process must not write to stderr
//! unless it was asked to, so an absent `HL_CUDA_LOG` leaves the mask exactly where it is today: closed.

use hl_log::{Config, Level, Tags};
use std::sync::Once;

/// Tag list to open, e.g. `HL_CUDA_LOG=cuda,shim` or `HL_CUDA_LOG=all`. Unset or `off` stays quiet.
const TAGS_VARIABLE: &str = "HL_CUDA_LOG";
/// Maximum severity to emit, e.g. `HL_CUDA_LOG_LEVEL=warn`. Defaults to `error`, the level a release
/// build still compiles in — asking for more in a release build is accepted and simply has less to
/// report, because the verbose macros are already gone.
const LEVEL_VARIABLE: &str = "HL_CUDA_LOG_LEVEL";

pub struct GuestLogging;

impl GuestLogging {
    /// Open the runtime gate from the environment, once per shared object.
    ///
    /// Each shim calls this from the single funnel every entry point takes before it can touch state or
    /// report anything (`ShimState::with`), so the gate is open before the first diagnostic. A single
    /// relaxed atomic on every call after the first.
    pub fn install() {
        static ONCE: Once = Once::new();
        ONCE.call_once(Self::apply_from_environment);
    }

    fn apply_from_environment() {
        let tags = std::env::var(TAGS_VARIABLE).ok();
        let level = std::env::var(LEVEL_VARIABLE).ok();
        Self::configure(tags.as_deref(), level.as_deref());
    }

    /// Open the gate for an explicit request, or leave it closed. Split from the environment read so the
    /// decision is testable without mutating process environment under a parallel test runner.
    pub fn configure(tags: Option<&str>, level: Option<&str>) {
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
    /// mask. The closed half is what the shipped driver did on every call: the mask is
    /// `AtomicU64::new(0)` and nothing in the guest ever opened it.
    #[test]
    fn error_reaches_the_sink_only_after_the_gate_is_opened() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();

        hl_log::Logging::global().set(Tags::NONE);
        GuestLogging::configure(Some("off"), None);
        hl_log::hl_error!(hl_log::tag::CUDA, "sentinel-while-closed");
        assert!(
            !captured("sentinel-while-closed"),
            "a closed mask must discard the line"
        );

        GuestLogging::configure(Some("cuda"), Some("error"));
        hl_log::hl_error!(hl_log::tag::CUDA, "sentinel-after-open");
        assert!(
            captured("sentinel-after-open"),
            "HL_CUDA_LOG=cuda must let a cuda-tagged error reach the sink"
        );

        // Opting one tag in never turns the whole driver loud.
        hl_log::hl_error!(hl_log::tag::PRESENT, "sentinel-unrequested-tag");
        assert!(
            !captured("sentinel-unrequested-tag"),
            "an unrequested tag must stay closed"
        );

        end_capture();
    }

    /// An absent variable must leave the gate exactly as it found it: an injected driver does not start
    /// writing to stderr merely because it was loaded.
    #[test]
    fn absent_or_empty_variable_leaves_the_gate_closed() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        start_capture();
        hl_log::Logging::global().set(Tags::NONE);

        for request in [None, Some(""), Some("off"), Some("none"), Some("nonsense")] {
            GuestLogging::configure(request, None);
            hl_log::hl_error!(hl_log::tag::CUDA, "sentinel-should-stay-quiet");
            assert!(
                !captured("sentinel-should-stay-quiet"),
                "{request:?} must leave the gate closed"
            );
        }

        end_capture();
    }

    /// The body the end-to-end test below runs in a FRESH process: install the composition root exactly
    /// as a shim's state funnel does, then report the failure a real entry point would. Ignored by
    /// default because it is only meaningful with `HL_CUDA_LOG` set and a virgin `Once`.
    #[test]
    #[ignore = "driven as a subprocess by install_reads_the_environment_end_to_end"]
    fn emit_sentinel_after_install() {
        use crate::model::device::DevicePtr;
        use crate::{CudaContext, CudaDeviceDesc};

        GuestLogging::install();
        // A REAL promoted site, not a synthetic one: freeing a pointer that is not a live allocation
        // base is a refused operation, and this is the line that says so.
        let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(1 << 20));
        let mut sink = hl_gpu::RecordingSink::with_full_caps();
        let _ = crate::service::allocate::mem_free(&mut ctx, &mut sink, DevicePtr(0xDEAD_0000));
    }

    /// End to end, in a real process, through the real stderr sink: setting `HL_CUDA_LOG` makes a driver
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
            run(Some("cuda")).contains("mem_free bad ptr"),
            "HL_CUDA_LOG=cuda must make the driver's error reach stderr"
        );
        assert!(
            !run(None).contains("mem_free bad ptr"),
            "with no HL_CUDA_LOG the driver must stay silent, exactly as it does today"
        );
    }
}
