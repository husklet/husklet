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

/// The `WgpuExecutor`-backed (lavapipe) host used by the real GRAPHICS tests — real SPIR-V/GLSL shaders
/// rasterized on the software Vulkan device, with the rendered target read back off the device. The
/// `CpuExecutor`-backed [`Executor`] below stays for the compute/identity real-app tests.
pub mod wgpu;

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Capabilities, Cmd, ConnectionHandler, CpuExecutor, FakeClock, GlobalLedger, Limits,
    ReadbackRequest, Session,
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
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        Self {
            session,
            exec,
            submits,
        }
    }
}

impl ConnectionHandler for RuntimeHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        self.submits.fetch_add(1, Ordering::Relaxed);
        let frame_bytes = hl_gpu::Encoder::stream(batch).len();
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
                            let _ =
                                hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            sock_path,
            stop,
            submits,
            _thread: thread,
        }
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

/// The shared output directory for the GL render-correctness demo PNGs (`/tmp/hl-demo/`). Created on
/// demand so a fresh checkout can run the demos without a manual `mkdir`.
pub fn demo_png_dir() -> PathBuf {
    let dir = PathBuf::from("/tmp/hl-demo");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Write an RGBA8 image (`w*h*4` bytes, row-major top-to-bottom) to `path` as a real PNG so a human can
/// eyeball what actually rasterized. A dependency-free encoder: one IDAT of raw-DEFLATE STORED blocks
/// (no compression, so no `flate2`/`miniz` needed) wrapped in a zlib stream, with correct CRC-32 chunk
/// checksums and an Adler-32 over the raw filtered scanlines. Small (64×64) demo frames make the STORED
/// overhead irrelevant while keeping the crate offline + dep-free.
pub fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    assert_eq!(
        rgba.len(),
        (w * h * 4) as usize,
        "write_png: rgba len must be w*h*4"
    );

    // ---- CRC-32 (IEEE, as PNG chunks require) --------------------------------------------------
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
    // ---- Adler-32 (the zlib trailer checksum over the uncompressed data) -----------------------
    fn adler32(bytes: &[u8]) -> u32 {
        let (mut a, mut b): (u32, u32) = (1, 0);
        for &byte in bytes {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]); // signature

    let chunk = |out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_in = Vec::with_capacity(4 + data.len());
        crc_in.extend_from_slice(kind);
        crc_in.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    };

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), no interlace.
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    // Raw scanlines, each prefixed with filter byte 0 (None).
    let mut raw: Vec<u8> = Vec::with_capacity((h * (1 + w * 4)) as usize);
    for y in 0..h {
        raw.push(0);
        let row = &rgba[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
        raw.extend_from_slice(row);
    }

    // zlib stream: 0x78 0x01 header, STORED DEFLATE blocks, Adler-32 trailer.
    let mut zlib: Vec<u8> = vec![0x78, 0x01];
    let mut off = 0usize;
    while off < raw.len() {
        let n = (raw.len() - off).min(0xFFFF);
        let last = (off + n >= raw.len()) as u8;
        zlib.push(last); // BFINAL in bit0, BTYPE=00 (stored)
        zlib.extend_from_slice(&(n as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(n as u16)).to_le_bytes());
        zlib.extend_from_slice(&raw[off..off + n]);
        off += n;
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, &out).expect("write PNG");
}

/// Convert a captured host render target (BGRA8, the GL default `Bgra8Unorm` order `read_texture` returns)
/// to RGBA8 for [`write_png`]. Row order is preserved (the wgpu render target is already top-left origin,
/// so the PNG comes out upright).
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = vec![0u8; bgra.len()];
    for (o, px) in bgra.chunks_exact(4).enumerate() {
        let i = o * 4;
        rgba[i] = px[2];
        rgba[i + 1] = px[1];
        rgba[i + 2] = px[0];
        rgba[i + 3] = px[3];
    }
    rgba
}
