//! Guest→host GPU-exec transport: the wire the guest producer drives and the host executor drains.
//!
//! Faithful Rust port of the transport that lived inline in `gl_shim.c` (`exec_stream`, `write_full`,
//! the `renderD128` `DD_IOCTL_GPU_ALLOC`), plus the completion-wake seam (eventfd/futex) the future
//! shared-memory command ring will use. The *working* path today is a persistent `SOCK_STREAM` Unix
//! socket carrying, per frame, a 16-byte header + the encoded IR byte-stream, answered by a 1-byte ack.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::ir;

/// Default host GPU-exec socket path (overridable via `$DD_GPU_EXEC`); matches gl_shim.c.
pub const DEFAULT_EXEC_SOCK: &str = "/run/user/0/dd-gpu-0";

/// Per-frame execution-response bytes the host executor writes after replaying a submit (see
/// `dd-display`'s `run_executor`, which writes `[rendered as u8]`). This is v1 of the exec response
/// protocol: a single status byte. `ACK_OK` means the frame replayed and its render is committed; any
/// other value — notably `ACK_FAIL` — means the host rejected or failed the frame and the guest must NOT
/// treat it as presented. A later revision can widen this to a typed response (status + error detail +
/// residency signal) negotiated against [`crate::IR_WIRE_VERSION`]; keeping the contract explicit here is
/// what lets the guest reject a failure instead of committing a stale/partly-rendered frame.
pub const ACK_OK: u8 = 1;
/// The host executor's documented failure acknowledgement (replay error / missing surface).
pub const ACK_FAIL: u8 = 0;
/// Guest render node the `DD_IOCTL_GPU_ALLOC` ioctl targets.
pub const RENDER_NODE: &str = "/dev/dri/renderD128";

/// The `DD_IOCTL_GPU_ALLOC` request code and dma-buf constants (must match `dd_gpu.h` / the engine's
/// `mem.c` handler and gl_shim.c). These describe the guest↔engine ioctl ABI, not the dd-gpu IR.
pub const DD_IOCTL_GPU_ALLOC: u64 = 0xC020_DD01;
pub const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;
pub const DRM_FMT_XRGB8888: u32 = 0x3432_5258;

/// Mirror of the C `struct hl_gpu_alloc` the ioctl reads/writes. `#[repr(C)]` pins the field order and
/// padding so the 32-byte layout matches the engine handler byte-for-byte (0xC02**0**DD01 → 0x20 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuAlloc {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride: u32,
    pub id: u32,
    pub fd: i32,
    pub ptr: u64,
}

// ---- minimal libc surface (dependency-free, matching dd-gpu / dd-term-core discipline) -------------
extern "C" {
    fn ioctl(fd: i32, request: core::ffi::c_ulong, arg: *mut core::ffi::c_void) -> i32;
    fn eventfd(initval: core::ffi::c_uint, flags: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn syscall(num: core::ffi::c_long, ...) -> core::ffi::c_long;
}

/// `write(2)` all of `buf`, retrying short writes and `EINTR` (the `write_full` of gl_shim.c).
pub fn write_full(s: &mut UnixStream, buf: &[u8]) -> std::io::Result<()> {
    s.write_all(buf)
}

/// A rendered frame's target surface, as registered with the engine via [`renderd::alloc`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Surface {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The dma-buf fd the ioctl handed back (for the wayland `SCM_RIGHTS` commit); -1 when unset.
    pub fd: i32,
    /// The allocation generation for [`id`](Self::id), stamped by the host at allocation time. Because
    /// the engine recycles a macOS IOSurface id across allocations, the guest echoes this generation in
    /// the dmabuf modifier (`modifier_hi` bits 17..=31) so the compositor can reject a stale reference
    /// (a modifier whose generation no longer matches the id's live allocation). 0 == unversioned.
    pub generation: u32,
}

impl Surface {
    pub fn from_alloc(a: &GpuAlloc) -> Self {
        Surface {
            id: a.id,
            width: a.width,
            height: a.height,
            stride: a.stride,
            fd: a.fd,
            // The engine returns the allocation generation in the `format` field on OUTPUT (it is an
            // input-only field otherwise), keeping the 32-byte ioctl ABI unchanged. Mask to the 15-bit
            // modifier field. 0 (an old engine / the gl_shim oracle) stays unversioned.
            generation: a.format & 0x7fff,
        }
    }
}

/// Guest GPU-memory registration via the render node.
pub mod renderd {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    /// Allocate an engine-backed surface (rung-2 IOSurface/dma-buf) of `width`x`height`. Ports
    /// gl_shim.c's `open("/dev/dri/renderD128"); ioctl(DD_IOCTL_GPU_ALLOC,&g_surf)`.
    ///
    /// The opened render-node fd is intentionally leaked for process lifetime (as in gl_shim.c: the
    /// surface's dma-buf fd returned in `GpuAlloc::fd` must outlive it). Returns the filled `GpuAlloc`.
    pub fn alloc(width: u32, height: u32, format: u32) -> std::io::Result<GpuAlloc> {
        let f = OpenOptions::new().read(true).write(true).open(RENDER_NODE)?;
        let mut a = GpuAlloc { width, height, format, ..Default::default() };
        let rc = unsafe { ioctl(f.as_raw_fd(), DD_IOCTL_GPU_ALLOC as _, &mut a as *mut _ as *mut _) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        std::mem::forget(f); // keep the render node open for process lifetime
        Ok(a)
    }
}

/// A persistent connection to the host GPU-exec service.
///
/// One connection lives for the surface's whole lifetime — a frame is just `[hdr][ir]`+ack on the same
/// fd, so the host keeps its per-connection backend (shader/PSO/resource caches) warm across frames
/// (gl_shim.c's L2/L7.1). A dropped connection reconnects lazily on the next [`submit`](ExecConn::submit),
/// and any reconnect after the first advances [`ExecConn::generation`]. The connection consumes that
/// reset internally by replaying all acknowledged residency before it sends new work.
pub struct ExecConn {
    path: String,
    sock: Option<UnixStream>,
    connects: u64,
    residency_reset: bool,
    generation: u64,
    residency: ResidencyJournal,
    negotiated_capabilities: Option<Vec<u8>>,
}

const MAX_REPLAY_BYTES: usize = 64 << 20;

/// Commands acknowledged by the current executor and therefore required to reconstruct the next
/// executor. Keeping the ordered command history is deliberate: uploads and GPU copies/draws can
/// mutate resources, so a create-only cache is not authoritative. Presents and waits are observations,
/// not residency, and are never repeated.
struct ResidencyJournal {
    cmds: Vec<ir::Cmd>,
    bytes: usize,
    replayable: bool,
    /// Maximum encoded residency the channel will replay on reconnect. Past this the journal drops
    /// `replayable` and a reconnect reports a clean API loss instead of silently recovering a truncated
    /// resource set. Configurable so the over-budget transition is testable without a multi-MB fixture.
    max_bytes: usize,
}

impl Default for ResidencyJournal {
    fn default() -> Self {
        Self { cmds: Vec::new(), bytes: 0, replayable: false, max_bytes: MAX_REPLAY_BYTES }
    }
}

impl ResidencyJournal {
    #[cfg(test)]
    fn with_budget(max_bytes: usize) -> Self {
        Self { max_bytes, ..Self::default() }
    }

    fn record(&mut self, cmds: &[ir::Cmd]) {
        if !self.replayable && !self.cmds.is_empty() {
            return;
        }
        for cmd in cmds {
            if matches!(cmd, ir::Cmd::Present { .. } | ir::Cmd::WaitFence { .. }) {
                continue;
            }
            let encoded = ir::encode_stream(core::slice::from_ref(cmd));
            self.bytes = self.bytes.saturating_add(encoded.len());
            if self.bytes > self.max_bytes {
                self.replayable = false;
                return;
            }
            self.cmds.push(cmd.clone());
        }
        self.replayable = true;
    }

    fn replay_bytes(&self) -> std::io::Result<Vec<u8>> {
        if !self.replayable && !self.cmds.is_empty() {
            return Err(api_loss("executor residency exceeded replay budget"));
        }
        Ok(ir::encode_stream(&self.cmds))
    }
}

fn api_loss(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionAborted, format!("API/device/context lost: {message}"))
}

impl ExecConn {
    /// Connect target from `$DD_GPU_EXEC`, falling back to [`DEFAULT_EXEC_SOCK`].
    pub fn from_env() -> Self {
        let path = std::env::var("DD_GPU_EXEC").unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_string());
        Self::new(path)
    }

    pub fn new(path: impl Into<String>) -> Self {
        ExecConn {
            path: path.into(),
            sock: None,
            connects: 0,
            residency_reset: false,
            generation: 0,
            residency: ResidencyJournal::default(),
            negotiated_capabilities: None,
        }
    }

    /// Total successful connects over this channel's life; should be 1 for a healthy run.
    pub fn connects(&self) -> u64 {
        self.connects
    }

    /// Monotonic executor generation. It advances only after a successful socket connection.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Pin the negotiated backend profile to this connection. A changed profile while objects are
    /// resident cannot be recovered safely and is reported as API loss instead of replaying commands
    /// under different wire/shader/format semantics.
    pub fn set_negotiated_capabilities(
        &mut self,
        caps: &hl_gpu::backend::Capabilities,
    ) -> std::io::Result<()> {
        let signature = caps.to_handshake();
        if self.negotiated_capabilities.as_ref().is_some_and(|old| old != &signature)
            && !self.residency.cmds.is_empty()
        {
            return Err(api_loss("executor capabilities changed with live residency"));
        }
        self.negotiated_capabilities = Some(signature);
        Ok(())
    }

    /// Compatibility observer for callers predating internal replay. Successful reconnect recovery
    /// consumes this flag before `submit` returns, so producers normally observe `false`.
    pub fn take_residency_reset(&mut self) -> bool {
        core::mem::replace(&mut self.residency_reset, false)
    }

    fn ensure(&mut self) -> std::io::Result<&mut UnixStream> {
        if self.sock.is_none() {
            let s = UnixStream::connect(&self.path)?;
            // A RE-connect (not the first) means a fresh host backend with an EMPTY resource cache.
            if self.connects >= 1 {
                self.residency_reset = true;
            }
            self.connects += 1;
            self.generation += 1;
            self.sock = Some(s);
        }
        Ok(self.sock.as_mut().unwrap())
    }

    /// Submit one frame's encoded IR for `surface` and block until the host acks the render.
    ///
    /// Wire (matches gl_shim.c `exec_stream` and the host executor's reader): a 16-byte little-endian
    /// header `[surface.id, surface.width, surface.height, ir.len()]` followed by the `ir` bytes; the
    /// host replies with a single ack byte. On any I/O error the connection is torn down and retried
    /// once (the executor may have restarted).
    pub fn submit(&mut self, surface: &Surface, ir: &[u8]) -> std::io::Result<()> {
        let current = ir::decode_stream(ir).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("invalid GPU IR: {e}"))
        })?;

        let mut last_err = None;
        for _ in 0..2 {
            // The closure yields the host's ack byte on success. A transport (I/O) error is retried on a
            // fresh connection; a NACK is NOT — the host received the frame and reported failure, so the
            // connection is healthy and re-sending would double-submit.
            let r = (|| -> std::io::Result<u8> {
                self.ensure()?;
                let mut payload = Vec::new();
                if self.residency_reset {
                    payload = self.residency.replay_bytes()?;
                }
                payload.extend_from_slice(ir);
                let mut hdr = [0u8; 16];
                hdr[0..4].copy_from_slice(&surface.id.to_le_bytes());
                hdr[4..8].copy_from_slice(&surface.width.to_le_bytes());
                hdr[8..12].copy_from_slice(&surface.height.to_le_bytes());
                hdr[12..16].copy_from_slice(&(payload.len() as u32).to_le_bytes());
                let s = self.sock.as_mut().expect("ensure installed executor socket");
                s.write_all(&hdr)?;
                s.write_all(&payload)?;
                let mut ack = [0u8; 1];
                s.read_exact(&mut ack)?;
                Ok(ack[0])
            })();
            match r {
                Ok(ACK_OK) => {
                    self.residency_reset = false;
                    self.residency.record(&current);
                    return Ok(());
                }
                // The executor NACKed this frame (replay failed / surface missing). Surface it as an error
                // rather than letting the guest commit a stale or partly-rendered frame as if it presented.
                Ok(nack) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("host executor NACKed frame (ack={nack})"),
                    ));
                }
                Err(e) => {
                    self.sock = None; // reconnect on next attempt
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe)))
    }
}

/// Guest-side capability negotiation: decode the host executor's serialized capability descriptor and
/// check the guest's required [`FeatureRequest`](hl_gpu::backend::FeatureRequest) against it BEFORE the
/// guest advertises the matching API version/extension/format to the app. A clean typed error here lets
/// the guest advertise a lower coherent profile (or reject the backend) instead of emitting a command the
/// host would later reject as a runtime `BadTag`/`Unsupported` — the Phase-1 "negotiate before advertise"
/// contract. On success the fully-decoded [`Capabilities`](hl_gpu::backend::Capabilities) is returned so
/// the guest can build its advertised surface from the intersection of its front end and the backend.
pub fn negotiate_host_capabilities(
    handshake: &[u8],
    req: &hl_gpu::backend::FeatureRequest,
) -> crate::Result<hl_gpu::backend::Capabilities> {
    let caps = hl_gpu::backend::Capabilities::from_handshake(handshake)?;
    caps.negotiate(req)?;
    Ok(caps)
}

/// A frame's IR accumulator over the shared [`ir::Cmd`] contract.
///
/// The guest front-end pushes host-agnostic commands; [`finish`](FrameBuilder::finish) serializes them
/// with the SAME [`ir::encode_stream`] the host decodes — the anti-drift guarantee in one call.
#[derive(Default)]
pub struct FrameBuilder {
    cmds: Vec<ir::Cmd>,
}

impl FrameBuilder {
    pub fn new() -> Self {
        Self { cmds: Vec::new() }
    }
    pub fn push(&mut self, cmd: ir::Cmd) -> &mut Self {
        self.cmds.push(cmd);
        self
    }
    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
    pub fn clear(&mut self) {
        self.cmds.clear();
    }
    pub fn cmds(&self) -> &[ir::Cmd] {
        &self.cmds
    }
    /// Serialize the accumulated commands to the wire byte-stream (`iu8(tag)+fields`, concatenated),
    /// exactly what [`crate::ir::Cmd::decode`] / `hl_gpu::replay::replay_stream` consume.
    pub fn finish(&self) -> Vec<u8> {
        ir::encode_stream(&self.cmds)
    }
}

/// A completion doorbell for the future shared-memory command ring (the eventfd/futex wake gfxstream
/// uses). The current socket path blocks on the ack instead; this is the forward seam so `dd-shim-vk`
/// and a ring-mode `dd-shim-gl` can signal without re-inventing it.
pub struct Doorbell {
    fd: i32,
}

impl Doorbell {
    /// Create a semaphore-mode eventfd (EFD_CLOEXEC|EFD_SEMAPHORE = 0x80000|0x1).
    pub fn new() -> std::io::Result<Self> {
        let fd = unsafe { eventfd(0, 0x8_0001) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Doorbell { fd })
    }
    pub fn raw_fd(&self) -> i32 {
        self.fd
    }
}

impl Drop for Doorbell {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

/// Wake up to `n` waiters parked on the futex word at `addr` (`FUTEX_WAKE`, private). The completion
/// primitive for the shared-ring path; unused by the socket-ack path but part of the transport ABI so
/// siblings share it. `SYS_futex` = 98 on aarch64, 202 on x86_64.
///
/// # Safety
/// `addr` must point to a live, correctly-aligned `u32` shared with the host.
pub unsafe fn futex_wake(addr: *mut u32, n: i32) -> i64 {
    const FUTEX_WAKE_PRIVATE: i32 = 1 | 128;
    #[cfg(target_arch = "aarch64")]
    const SYS_FUTEX: core::ffi::c_long = 98;
    #[cfg(target_arch = "x86_64")]
    const SYS_FUTEX: core::ffi::c_long = 202;
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    const SYS_FUTEX: core::ffi::c_long = 202;
    syscall(SYS_FUTEX, addr, FUTEX_WAKE_PRIVATE, n) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SurfaceId;
    use std::os::unix::net::UnixListener;

    #[test]
    fn gpu_alloc_layout_is_32_bytes() {
        // The ioctl request 0xC020DD01 encodes a 0x20-byte payload; the struct must match.
        assert_eq!(core::mem::size_of::<GpuAlloc>(), 0x20);
    }

    #[test]
    fn framebuilder_encodes_the_shared_contract() {
        // Encode a couple of commands through the shim's FrameBuilder, then decode them back with the
        // HOST's own decoder. Same bytes, same code path — the anti-drift proof.
        let mut fb = FrameBuilder::new();
        fb.push(ir::Cmd::CreateFence(7))
            .push(ir::Cmd::Present { surface: 1, texture: 500 });
        let bytes = fb.finish();

        let mut d = crate::wire::Decoder::new(&bytes);
        let a = ir::Cmd::decode(&mut d).unwrap();
        let b = ir::Cmd::decode(&mut d).unwrap();
        assert!(d.is_empty());
        assert!(matches!(a, ir::Cmd::CreateFence(7)));
        assert!(matches!(b, ir::Cmd::Present { surface: 1, texture: 500 }));
        let _ = SurfaceId(1);
    }

    #[test]
    fn exec_conn_frames_header_then_ir_then_reads_ack() {
        // Stand up an in-process "host executor" on a socketpair-style Unix listener and assert the
        // ExecConn writes exactly [id,w,h,len][ir] and consumes one ack byte.
        let dir = std::env::temp_dir().join(format!("dd-shim-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("exec.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let ir_bytes = FrameBuilder::new().finish(); // empty frame is fine for framing test
        let ir_clone = ir_bytes.clone();
        let sock2 = sock.clone();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut hdr = [0u8; 16];
            c.read_exact(&mut hdr).unwrap();
            let id = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let w = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
            let h = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
            let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
            let mut body = vec![0u8; len];
            c.read_exact(&mut body).unwrap();
            c.write_all(&[1u8]).unwrap(); // ack
            (id, w, h, body)
        });

        let mut conn = ExecConn::new(sock2.to_string_lossy().into_owned());
        let surf = Surface { id: 42, width: 640, height: 480, stride: 2560, fd: -1, ..Default::default() };
        conn.submit(&surf, &ir_clone).unwrap();
        let (id, w, h, body) = server.join().unwrap();
        assert_eq!((id, w, h), (42, 640, 480));
        assert_eq!(body, ir_clone);
        assert_eq!(conn.connects(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exec_conn_rejects_a_failure_ack() {
        // Mirrors the tracked gate `executor_transport_rejects_a_failed_frame_acknowledgement`: a host that
        // reads the frame and replies ACK_FAIL (0) must make submit() return Err, not be treated as a
        // successful present.
        let dir = std::env::temp_dir().join(format!("dd-shim-nack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("nack.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut hdr = [0u8; 16];
            c.read_exact(&mut hdr).unwrap();
            let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
            let mut body = vec![0u8; len];
            c.read_exact(&mut body).unwrap();
            c.write_all(&[super::ACK_FAIL]).unwrap(); // documented failure ack
        });
        let mut conn = ExecConn::new(sock.to_string_lossy().into_owned());
        let surf = Surface { id: 7, width: 16, height: 9, stride: 64, fd: -1, ..Default::default() };
        let valid_ir = ir::encode_stream(&[ir::Cmd::CreateFence(1)]);
        let result = conn.submit(&surf, &valid_ir);
        server.join().unwrap();
        assert!(result.is_err(), "ExecConn treated ACK_FAIL as success");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconnect_replays_acknowledged_residency_once_before_new_work() {
        use crate::ir::{buffer_usage, BufferDesc, Cmd, CommandBuffer};

        fn read_frame(c: &mut UnixStream) -> Vec<u8> {
            let mut hdr = [0u8; 16];
            c.read_exact(&mut hdr).unwrap();
            let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
            let mut body = vec![0; len];
            c.read_exact(&mut body).unwrap();
            body
        }

        let dir = std::env::temp_dir().join(format!("dd-shim-reconnect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("exec.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let upload = vec![
            Cmd::CreateBuffer(
                4,
                BufferDesc { size: 4, usage: buffer_usage::COPY_DST, label: "resident".into() },
            ),
            Cmd::WriteBuffer { id: 4, offset: 0, data: vec![1, 2, 3, 4] },
        ];
        let draw = vec![Cmd::Submit(CommandBuffer::default()), Cmd::Present { surface: 9, texture: 8 }];
        let upload_bytes = ir::encode_stream(&upload);
        let draw_bytes = ir::encode_stream(&draw);

        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let first_body = read_frame(&mut first);
            first.write_all(&[ACK_OK]).unwrap();
            drop(first); // fault after upload acknowledgement, before the draw

            let (mut second, _) = listener.accept().unwrap();
            let recovered = read_frame(&mut second);
            second.write_all(&[ACK_OK]).unwrap();
            (first_body, recovered)
        });

        let surf = Surface { id: 9, width: 8, height: 8, stride: 32, fd: -1, ..Default::default() };
        let mut conn = ExecConn::new(sock.to_string_lossy());
        conn.submit(&surf, &upload_bytes).unwrap();
        conn.submit(&surf, &draw_bytes).unwrap();

        let (first, recovered) = server.join().unwrap();
        assert_eq!(ir::decode_stream(&first).unwrap(), upload);
        let recovered = ir::decode_stream(&recovered).unwrap();
        assert_eq!(&recovered[..upload.len()], upload.as_slice(), "residency first, exactly once");
        assert_eq!(&recovered[upload.len()..], draw.as_slice(), "new work follows replay");
        assert_eq!(conn.generation(), 2);
        assert!(!conn.take_residency_reset(), "successful replay consumed the reset generation");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ir_wire_version_tracks_dd_gpu() {
        // IR_WIRE_VERSION is bound to the source of truth so guest/host can't disagree on the tag set.
        assert_eq!(crate::IR_WIRE_VERSION, hl_gpu::ir::WIRE_VERSION);
    }

    #[test]
    fn guest_negotiates_a_serialized_host_capability_descriptor() {
        use hl_gpu::backend::{command_bits, format_bits, shader_payload, FeatureRequest, GpuBackend};
        use hl_gpu::ir::{etag, TextureFormat, WIRE_VERSION};

        // A real host advertisement (the software oracle: PTX shaders, color formats, all commands),
        // serialized to the handshake wire form the guest would read off the connection.
        let handshake = hl_gpu::software::SoftwareBackend::new().capabilities().to_handshake();

        // Compatible request → negotiation succeeds and returns the decoded descriptor.
        let ok = FeatureRequest {
            wire_version: WIRE_VERSION,
            shader_payloads: shader_payload::PTX,
            command_bits: command_bits(&[etag::COPY_T2T, etag::DISPATCH]),
            texture_formats: format_bits(&[TextureFormat::Rgba8Unorm]),
        };
        let caps = negotiate_host_capabilities(&handshake, &ok).expect("compatible negotiation");
        assert_eq!(caps.wire_version, WIRE_VERSION);

        // Incompatible: guest needs an MSL shader payload the software backend cannot execute → clean error.
        let wants_msl = FeatureRequest { shader_payloads: shader_payload::MSL, ..ok.clone() };
        assert!(negotiate_host_capabilities(&handshake, &wants_msl).is_err());

        // Incompatible: guest needs a depth format the software oracle does not materialize → clean error.
        let wants_depth = FeatureRequest { texture_formats: format_bits(&[TextureFormat::Depth32Float]), ..ok };
        assert!(negotiate_host_capabilities(&handshake, &wants_depth).is_err());
    }

    #[test]
    fn capability_change_with_live_residency_is_typed_api_loss() {
        use hl_gpu::backend::GpuBackend;

        let mut conn = ExecConn::new("unused");
        let caps = hl_gpu::software::SoftwareBackend::new().capabilities();
        conn.set_negotiated_capabilities(&caps).unwrap();
        conn.residency.record(&[ir::Cmd::CreateFence(1)]);

        let mut changed = caps;
        changed.wire_version += 1;
        let err = conn.set_negotiated_capabilities(&changed).expect_err("live profile change is loss");
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionAborted);
        assert!(err.to_string().contains("API/device/context lost"));
    }

    #[test]
    fn killed_executor_reconnect_replays_complete_residency_recovers_pixels_and_stays_bounded() {
        // Row `executor_reconnect_replays_complete_residency_or_reports_api_loss`: kill a LIVE executor
        // mid-stream (after it acknowledges residency, before the dependent draw), then prove the
        // reconnected executor recovers byte-identical pixels from the replayed residency while its
        // resource footprint stays bounded.
        use hl_gpu::backend::GpuBackend;
        use hl_gpu::ir::{buffer_usage, BufferDesc, Cmd, CommandBuffer, Enc};
        use hl_gpu::limits::{ExecutorBudget, GlobalBudget, ReplayLimits};
        use hl_gpu::software::SoftwareBackend;

        const DATA: [u8; 16] = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 11, 22, 33, 44, 55, 66];

        fn read_frame(c: &mut UnixStream) -> Vec<u8> {
            let mut hdr = [0u8; 16];
            c.read_exact(&mut hdr).unwrap();
            let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
            let mut body = vec![0; len];
            c.read_exact(&mut body).unwrap();
            body
        }

        let dir = std::env::temp_dir().join(format!("dd-shim-kill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("exec.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        // Residency the later copy depends on: two buffers + an upload into `src`.
        let residency = vec![
            Cmd::CreateBuffer(1, BufferDesc {
                size: 16,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: "src".into(),
            }),
            Cmd::CreateBuffer(2, BufferDesc {
                size: 16,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: "dst".into(),
            }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: DATA.to_vec() },
        ];
        // New work that only produces the right bytes if residency was FULLY recovered: copy src -> dst.
        let new_work = vec![Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer { src: 1, src_offset: 0, dst: 2, dst_offset: 0, size: 16 }],
            signal: None,
        })];
        let residency_bytes = ir::encode_stream(&residency);
        let new_work_bytes = ir::encode_stream(&new_work);

        // Sanity: the new work alone — WITHOUT the replayed residency — cannot recover the pixels, since
        // the copy references a buffer a fresh executor never created. This is what makes replay
        // load-bearing rather than incidental.
        {
            let mut empty = SoftwareBackend::new();
            assert!(
                hl_gpu::replay::replay_stream(&mut empty, &new_work_bytes).is_err(),
                "copy must fail against an executor that lost residency"
            );
        }

        let server = std::thread::spawn(move || {
            // Connection #1: accept residency, ACK, then DIE before the copy arrives (executor killed).
            let (mut first, _) = listener.accept().unwrap();
            let first_body = read_frame(&mut first);
            first.write_all(&[ACK_OK]).unwrap();
            drop(first);

            // Connection #2: a brand-new executor (empty cache + fresh residency accounting). It must get
            // the complete residency replayed ahead of the new work, rebuild `dst`, and stay bounded.
            let (mut second, _) = listener.accept().unwrap();
            let recovered = read_frame(&mut second);

            let global = GlobalBudget::new(1 << 20, 64);
            let limits = ReplayLimits::from_capabilities(SoftwareBackend::new().capabilities());
            let mut budget = ExecutorBudget::new(limits, global);
            let mut backend = SoftwareBackend::new();
            let replayed = hl_gpu::replay::replay_stream_limited(&mut backend, &recovered, &mut budget);

            let mut dst = [0u8; 16];
            let readback = backend.read_buffer(hl_gpu::id::BufferId(2), 0, &mut dst);

            // Bounded: exactly the two live buffers (32 bytes) are charged — reconnect recovery does not
            // inflate the object count or leak bytes.
            let bounded = budget.object_count() == 2 && budget.residency_bytes() == 32;
            let ok = replayed.is_ok() && readback.is_ok() && dst == DATA && bounded;
            second.write_all(&[if ok { ACK_OK } else { ACK_FAIL }]).unwrap();
            (first_body, recovered, dst, budget.object_count(), budget.residency_bytes(), ok)
        });

        let surf = Surface { id: 9, width: 4, height: 4, stride: 16, fd: -1, ..Default::default() };
        let mut conn = ExecConn::new(sock.to_string_lossy());
        conn.submit(&surf, &residency_bytes).expect("residency frame acknowledged");
        // The executor died; this submit transparently reconnects, replays residency, then sends new work.
        // It only returns Ok because the reconnected executor verified recovery + bounded resources in its ACK.
        conn.submit(&surf, &new_work_bytes).expect("reconnect recovered residency and executor ACKed");

        let (first_body, recovered, dst, objects, bytes, ok) = server.join().unwrap();
        assert_eq!(ir::decode_stream(&first_body).unwrap(), residency, "first executor saw the residency frame");
        let recovered_cmds = ir::decode_stream(&recovered).unwrap();
        assert_eq!(&recovered_cmds[..residency.len()], residency.as_slice(), "complete residency replayed first");
        assert_eq!(&recovered_cmds[residency.len()..], new_work.as_slice(), "new work follows the replay");
        assert_eq!(dst, DATA, "recovered pixels are byte-identical to the pre-kill upload");
        assert_eq!((objects, bytes), (2, 32), "reconnect recovery stayed bounded");
        assert!(ok, "executor verified recovered pixels + bounded resources");
        assert_eq!(conn.connects(), 2, "exactly one reconnect after the kill");
        assert_eq!(conn.generation(), 2);
        assert!(!conn.take_residency_reset(), "successful replay consumed the reset generation");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn residency_over_replay_budget_reports_clean_api_loss() {
        // The other side of the row: when acknowledged residency exceeds the channel's replay budget, a
        // reconnect must report a clean, typed API loss instead of silently recovering a truncated set.
        use hl_gpu::ir::{buffer_usage, BufferDesc, Cmd};
        let mk = |id| {
            Cmd::CreateBuffer(id, BufferDesc { size: 16, usage: buffer_usage::COPY_DST, label: String::new() })
        };
        let mut journal = ResidencyJournal::with_budget(30);
        journal.record(&[mk(1)]);
        assert!(journal.replay_bytes().is_ok(), "residency within budget replays");
        journal.record(&[mk(2)]); // pushes the encoded journal past the replay budget
        let err = journal.replay_bytes().expect_err("over-budget residency must not silently truncate");
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionAborted);
        assert!(err.to_string().contains("API/device/context lost"));
    }
}
