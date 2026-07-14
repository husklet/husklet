//! Shared host-side plumbing for the real-app / real-software tests: a GPU executor served over a unix
//! socket, plus staged-shim locators.
//!
//! Every real-software test points a REAL program's guest shim at `$HL_GPU_EXEC` (a unix socket) and this
//! module is the host end of that socket: a runtime `Session` + reference `CpuExecutor` (with the CUDA PTX
//! front-end injected as the kernel compiler, matching what the composition root would supply) served by
//! `hl_gpu::serve_connection_with_handler`. Lifted verbatim from the former `hl_wip-realsw` crate's
//! `src/lib.rs` so all the migrated tests reuse it.
//!
//! Some tests only use a subset of these helpers; `#![allow(dead_code)]` keeps each test binary quiet
//! about the parts it does not touch (each `mod common;` compiles the whole module).
#![allow(dead_code)]

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    encode_stream, BufferId, Capabilities, Cmd, ConnectionHandler, CpuExecutor, FakeClock,
    GlobalLedger, Limits, ReadbackRequest, Session,
};

/// A host that owns a runtime `Session` + a `CpuExecutor` with the CUDA PTX kernel compiler injected, and
/// serves BOTH the submit path (through the runtime pipeline) and device→host readback. One `&mut self`
/// drives both halves.
pub struct RuntimeHost {
    session: Session,
    exec: CpuExecutor,
    /// Count of submitted batches — lets a test assert the guest actually drove the executor.
    submits: Arc<AtomicU64>,
}

impl RuntimeHost {
    pub fn new(submits: Arc<AtomicU64>) -> Self {
        let mut exec = CpuExecutor::new();
        // Inject the driver's PTX parser so a shim-produced kernel payload compiles on the fly. Harmless
        // for the GL/Vulkan graphics tests (they never create a PtxKernel), required for CUDA.
        exec.set_kernel_compiler(|desc: &KernelDescriptor| {
            hl_cuda::adapter::ptx::compile(&desc.ptx, &desc.entry, desc.block)
        });
        // Serve with a permissive capability set so the real graphics/compute lowering (SPIR-V shader
        // modules, render targets, etc.) is accepted by the runtime validator.
        let limits = Limits::from_capabilities(Capabilities::full("host"));
        let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
        Self { session, exec, submits }
    }
}

impl ConnectionHandler for RuntimeHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        self.submits.fetch_add(1, Ordering::Relaxed);
        let frame_bytes = encode_stream(batch).len();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
            Ok(_) => Verdict::Ack,
            Err(_) => Verdict::Nack,
        }
    }

    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.exec,
            BufferId(req.id),
            req.offset,
            req.len as usize,
        )
        .ok()
    }
}

/// A running host GPU executor: a background thread accepts guest connections on a temp unix socket and
/// serves each one with its own `RuntimeHost`. Drops the socket file on `Drop`.
pub struct Executor {
    pub sock_path: PathBuf,
    stop: Arc<AtomicBool>,
    /// Total batches submitted across all connections served so far.
    submits: Arc<AtomicU64>,
    _thread: thread::JoinHandle<()>,
}

impl Executor {
    /// Bind a fresh temp socket, start accepting guest connections, and return once the socket exists so a
    /// caller can immediately `HL_GPU_EXEC=<path>` a subprocess at it.
    pub fn start(tag: &str) -> Self {
        let sock_path = std::env::temp_dir().join(format!(
            "hl-wip-{tag}-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind executor socket");
        listener
            .set_nonblocking(true)
            .expect("nonblocking executor socket");

        let stop = Arc::new(AtomicBool::new(false));
        let submits = Arc::new(AtomicU64::new(0));

        let stop_t = Arc::clone(&stop);
        let submits_t = Arc::clone(&submits);
        let thread = thread::spawn(move || {
            // Accept-loop: each guest process opens one persistent connection; serve each on its own
            // thread so multiple/staggered clients (and reconnects) all work.
            while !stop_t.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        let submits_c = Arc::clone(&submits_t);
                        thread::spawn(move || {
                            let caps = Capabilities::full("host");
                            let mut host = RuntimeHost::new(submits_c);
                            let _ = hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { sock_path, stop, submits, _thread: thread }
    }

    /// Path string for `HL_GPU_EXEC`.
    pub fn sock(&self) -> String {
        self.sock_path.to_string_lossy().into_owned()
    }

    /// Total protocol batches the guest(s) have submitted so far.
    pub fn submit_count(&self) -> u64 {
        self.submits.load(Ordering::Relaxed)
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Absolute path to a staged aarch64 shim directory, e.g. `staged_dir("gl")` → `~/.hl/gl/aarch64`.
pub fn staged_dir(driver: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME set");
    PathBuf::from(home).join(".hl").join(driver).join("aarch64")
}
