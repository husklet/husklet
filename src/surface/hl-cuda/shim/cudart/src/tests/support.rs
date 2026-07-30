//! Shared test doubles: the runtime error codes, a minimal fatbin container, and the in-process
//! executor host that serves the socket transport.

use core::ffi::c_void;

pub(super) const CUDART_ERR_INVALID_VALUE: i32 = 1;
pub(super) const CUDART_ERR_INVALID_DEVICE: i32 = 101;
pub(super) const CUDART_ERR_INVALID_RESOURCE_HANDLE: i32 = 400;
pub(super) const CUDART_ERR_NOT_READY: i32 = 600;

/// The default-stream handle (null token).
pub(super) fn stream_default() -> *mut c_void {
    core::ptr::null_mut()
}

/// Wrap PTX text in a minimal nvcc-style fatbin container (one uncompressed PTX entry) — the exact
/// shape `__cudaRegisterFatBinary`'s `container_bytes` walks (bare container, magic 0xba55ed50).
pub(super) fn make_fatbin(ptx: &str) -> Vec<u8> {
    let payload = ptx.as_bytes();
    let payload_len = payload.len() as u64;
    let fat_size = 64u64 + payload_len; // one 64-byte entry header + the payload
    let mut c = Vec::new();
    c.extend_from_slice(&0xba55_ed50u32.to_le_bytes()); // magic
    c.extend_from_slice(&1u16.to_le_bytes()); // version
    c.extend_from_slice(&16u16.to_le_bytes()); // header_size
    c.extend_from_slice(&fat_size.to_le_bytes()); // fat_size
    let mut e = [0u8; 64];
    e[0..2].copy_from_slice(&1u16.to_le_bytes()); // kind = PTX
    e[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
    e[8..16].copy_from_slice(&payload_len.to_le_bytes()); // payload_size (flags @40 stay 0 → uncompressed)
    c.extend_from_slice(&e);
    c.extend_from_slice(payload);
    c
}

/// A runtime-backed host that serves BOTH the submit path (through a runtime `Session` + reference
/// `CpuExecutor`) and the device→host readback path over the real socket transport — so the cudart
/// shim's process-global `RemoteCommandSink` has a live executor for `cudaMalloc`/`cudaMemset`/
/// `cudaMemcpy` to actually land against (mirrors `hl-gpu/tests/readback.rs::RuntimeHost`).
pub(super) struct RuntimeHost {
    pub(super) session: hl_gpu::Session,
    pub(super) exec: hl_gpu::CpuExecutor,
}
impl hl_gpu::ConnectionHandler for RuntimeHost {
    fn submit(
        &mut self,
        _h: &hl_gpu::transport::SubmitHeader,
        batch: &[hl_gpu::Cmd],
    ) -> hl_gpu::transport::Verdict {
        let frame_bytes = hl_gpu::Encoder::stream(batch).len();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
            Ok(_) => hl_gpu::transport::Verdict::Ack,
            Err(_) => hl_gpu::transport::Verdict::Nack,
        }
    }
    fn read_buffer(&mut self, req: &hl_gpu::ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.exec,
            hl_gpu::BufferId(req.id),
            req.offset,
            req.len as usize,
        )
        .ok()
    }
    /// `cudaDeviceSynchronize`/`cudaStreamSynchronize` lower to a timeline-fence barrier, so the host
    /// must answer the fence queries too — leaving them at the trait default returns `None`, which the
    /// client reports as a transport failure rather than a completed barrier.
    fn poll_fence(&mut self, req: &hl_gpu::ReadbackRequest) -> Option<bool> {
        hl_gpu::runtime::service::dispatch::poll_fence(
            &self.session,
            &mut self.exec,
            hl_gpu::FenceId(req.id),
            req.offset,
        )
        .ok()
    }
    fn wait_fence(&mut self, req: &hl_gpu::ReadbackRequest) -> Option<hl_gpu::FenceWait> {
        hl_gpu::runtime::service::dispatch::wait_timeout(
            &mut self.session,
            &mut self.exec,
            hl_gpu::FenceId(req.id),
            req.offset,
            req.len,
        )
        .ok()
    }
}

// A `cudaMemset`/`cudaMemsetAsync` whose `count` far exceeds the destination allocation must return the
// truthful `cudaErrorInvalidValue` WITHOUT building the `vec![value; count]` fill buffer first (a hostile
// `count` near `usize::MAX` would otherwise be an unbounded multi-GiB host alloc → OOM-abort). A legal
// memset must still write EXACTLY the right bytes — asserted via a real device→host readback.
