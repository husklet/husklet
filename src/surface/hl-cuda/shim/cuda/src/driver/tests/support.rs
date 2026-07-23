use super::*;
use crate::state::reset;
use std::sync::{Mutex, MutexGuard, OnceLock};
/// Serialize the tests: they share one process-global `State`, so they must not run concurrently.
pub(super) fn guard() -> MutexGuard<'static, ()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    let g = L
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    reset();
    g
}

/// Record a live device allocation of `size` bytes directly in the model (no sink), returning its
/// device pointer — the stand-in for a completed `cuMemAlloc`.
pub(super) fn record_alloc(size: u64) -> u64 {
    ShimState::with(|s| {
        let b = s.ctx.alloc_buffer();
        s.ctx.mem.insert(b, size).0
    })
}

/// Record a live *managed* allocation directly in the model (no sink) — the stand-in for a completed
/// `cuMemAllocManaged`.
pub(super) fn record_managed_alloc(size: u64) -> u64 {
    ShimState::with(|s| {
        let b = s.ctx.alloc_buffer();
        s.ctx.mem.insert_managed(b, size).0
    })
}
pub(super) fn load_vecadd() -> *mut c_void {
    let img = std::ffi::CString::new(ptx::VECADD_PTX).unwrap();
    let mut module: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleLoadData(&mut module, img.as_ptr() as *const c_void),
        CUDA_SUCCESS
    );
    let name = std::ffi::CString::new("vecadd").unwrap();
    let mut func: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleGetFunction(&mut func, module, name.as_ptr()),
        CUDA_SUCCESS
    );
    assert!(!func.is_null());
    func
}

use std::sync::atomic::{AtomicUsize, Ordering};

/// A `CUhostFn` test double: bumps the `AtomicUsize` its `userData` points at.
pub(super) extern "C" fn host_cb(data: *mut c_void) {
    let c = unsafe { &*(data as *const AtomicUsize) };
    c.fetch_add(1, Ordering::SeqCst);
}
/// A `CUstreamCallback` test double: bumps the counter only when handed `CUDA_SUCCESS`.
pub(super) extern "C" fn stream_cb(_s: *mut c_void, status: i32, data: *mut c_void) {
    if status == CUDA_SUCCESS {
        let c = unsafe { &*(data as *const AtomicUsize) };
        c.fetch_add(1, Ordering::SeqCst);
    }
}

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
}

pub(super) fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
pub(super) fn as_f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}
