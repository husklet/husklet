//! Execution proof for the error-level transport diagnostics.
//!
//! `hl-log` has two independent gates and this crate sat behind both. Compile-time, a default release build
//! strips `warn`/`info`/`debug`/`trace` and keeps only `hl_error!` — and the transport's most explanatory
//! lines (the host's frame rejection, a contained handler panic, a failed decode) were all `warn`, so a
//! shipped `hl-gpu` on EITHER side of the socket could not report why a frame died. Runtime, the tag mask
//! starts closed and is only opened by a composition root.
//!
//! Asserting this in-process is not possible: the mask and the sink are process-global. So, following the
//! house standard, these tests re-execute THIS test binary with and without the variable and read the
//! child's stderr, and they drive a REAL path — a genuine `UnixListener` with a `RemoteCommandSink`
//! submitting a real batch — rather than a synthetic sentinel.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;

use hl_gpu::transport::Verdict;
use hl_gpu::{serve_connection, Capabilities, Cmd, CommandSink, RemoteCommandSink};

/// Tag list to open, mirroring what a guest driver's composition root reads from its own variable.
const TAGS_VARIABLE: &str = "HL_GPU_TEST_LOG";

/// The composition root this crate does NOT have: `hl-gpu` is a library, so on the host the app opens the
/// mask and in a guest process the driver shim's `GuestLogging::install` does. A test process has neither,
/// so it stands one up exactly the way those roots do.
fn install_root() {
    let Ok(requested) = std::env::var(TAGS_VARIABLE) else {
        return;
    };
    let logging: hl_log::Tags = requested.parse().unwrap_or(hl_log::Tags::NONE);
    if logging == hl_log::Tags::NONE {
        return;
    }
    hl_log::Config {
        logging,
        level: hl_log::Level::Error,
        profiling: hl_log::Tags::NONE,
    }
    .apply();
}

struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("hl-gpu-diag-{tag}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        TempSock(p)
    }
    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
impl Drop for TempSock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Submit a real batch over a real socket to a host that answers `verdict`, driving BOTH the server's
/// refusal path and the client's rejected-ack path. Returns once the exchange has completed.
fn submit_one(tag: &str, handler: impl Fn() -> Verdict + Send + 'static) {
    let sock = TempSock::new(tag);
    let listener = UnixListener::bind(&sock.0).expect("bind the test socket");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept the client");
        let caps = Capabilities::permissive_fixture("diagnostics");
        let _ = serve_connection(&stream, &caps, move |_, _| handler());
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    // A real command, encoded and framed by the real codec — not a hand-built frame.
    let result = sink.submit(&[Cmd::CreateFence(1)]);
    assert!(result.is_err(), "the host refused this frame");
    drop(sink);
    let _ = server.join();
}

/// Subprocess body: the host REFUSES the frame. Exercises the server's `frame REFUSED` site and the
/// client's `host executor REJECTED frame` site in one exchange.
#[test]
#[ignore = "driven as a subprocess by promoted_sites_print_only_when_the_gate_is_open"]
fn emit_rejected_frame() {
    install_root();
    submit_one("nack", || Verdict::Nack);
}

/// Subprocess body: the handler PANICS. The wire byte is the same `ACK_FAIL` a clean refusal writes, so
/// the only thing that can distinguish them afterwards is the promoted line and its panic message.
#[test]
#[ignore = "driven as a subprocess by promoted_sites_print_only_when_the_gate_is_open"]
fn emit_panicking_handler() {
    install_root();
    submit_one("panic", || panic!("executor exploded on a real batch"));
}

fn run_child(body: &str, tags: Option<&str>) -> String {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary path"));
    command.args(["--exact", "--ignored", "--nocapture", body]);
    match tags {
        Some(value) => command.env(TAGS_VARIABLE, value),
        None => command.env_remove(TAGS_VARIABLE),
    };
    let output = command.output().expect("re-exec the test binary");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the subprocess body must have run: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// End to end in real processes through the real stderr sink: the promoted sites print when the tag mask
/// is opened and stay silent when it is not. The silent half is what the shipped library did on every
/// frame — the line existed, at `warn`, behind a mask nobody opened.
#[test]
fn promoted_sites_print_only_when_the_gate_is_open() {
    let open = run_child("emit_rejected_frame", Some("transport,wire"));
    assert!(
        open.contains("host executor REJECTED frame"),
        "the client's rejection line must arrive:\n{open}"
    );
    assert!(
        open.contains("frame REFUSED"),
        "the host's refusal line must arrive:\n{open}"
    );

    let closed = run_child("emit_rejected_frame", None);
    assert!(
        !closed.contains("REJECTED") && !closed.contains("REFUSED"),
        "an unopened mask must discard both lines:\n{closed}"
    );
}

/// A contained panic must be distinguishable from an ordinary nack, and must carry its message. Both
/// answer the same ack byte, so this is a correctness-of-diagnosis property, not a verbosity one.
#[test]
fn a_panicking_handler_is_named_distinctly_from_a_refusal() {
    let panicked = run_child("emit_panicking_handler", Some("transport"));
    assert!(
        panicked.contains("submit handler PANICKED"),
        "the panic must be named as a panic:\n{panicked}"
    );
    assert!(
        panicked.contains("executor exploded on a real batch"),
        "the panic MESSAGE must survive the boundary:\n{panicked}"
    );

    // The clean refusal must NOT claim a panic, or the distinction is worthless.
    let refused = run_child("emit_rejected_frame", Some("transport"));
    assert!(
        !refused.contains("PANICKED"),
        "a clean refusal must not be reported as a panic:\n{refused}"
    );
}

/// Subprocess body: 64 refused frames on ONE connection, the shape a persistently failing host has.
#[test]
#[ignore = "driven as a subprocess by a_persistently_refusing_host_does_not_flood"]
fn emit_repeated_rejections() {
    install_root();
    let sock = TempSock::new("flood");
    let listener = UnixListener::bind(&sock.0).expect("bind the test socket");
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept the client");
        let caps = Capabilities::permissive_fixture("diagnostics");
        let _ = serve_connection(&stream, &caps, |_, _| Verdict::Nack);
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    let mut refusals = 0;
    for i in 0..64u32 {
        if sink.submit(&[Cmd::CreateFence(i + 1)]).is_err() {
            refusals += 1;
        }
    }
    assert_eq!(refusals, 64, "every frame really was refused");
    drop(sink);
    let _ = server.join();
}

/// The per-frame sites are LATCHED: a host that refuses every frame produces a handful of legible lines,
/// not one per frame. Without this a persistent failure at frame rate buries its own first occurrence and
/// the promotion trades one useless extreme (never printing) for the other.
#[test]
fn a_persistently_refusing_host_does_not_flood() {
    let out = run_child("emit_repeated_rejections", Some("transport,wire"));
    let count = |needle: &str| out.matches(needle).count();
    assert_eq!(
        count("host executor REJECTED frame"),
        1,
        "64 refused frames, one ack value, one connection → exactly one client line:\n{out}"
    );
    assert_eq!(
        count("frame REFUSED"),
        1,
        "…and exactly one host line:\n{out}"
    );
}
