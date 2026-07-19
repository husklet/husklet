//! The CUDA **Runtime API** launch path: the `__cudaRegister*` fatbin/function registry + the
//! `cudaLaunchKernel` lowering. This is the runtime-API counterpart to the driver API's explicit
//! `cuModuleLoadData` → `cuModuleGetFunction` → `cuLaunchKernel`.
//!
//! nvcc emits host stubs that, at image load, call `__cudaRegisterFatBinary(fatbin)` → an opaque handle,
//! then `__cudaRegisterFunction(handle, hostFn, …, deviceName, …)` once per `__global__` kernel to bind a
//! HOST function pointer to a DEVICE entry name, closed by `__cudaRegisterFatBinaryEnd(handle)`. A later
//! `cudaLaunchKernel(hostFn, grid, block, args, …)` names the kernel only by that host pointer.
//!
//! The [`Registry`] is the missing map: a fatbin handle → the loaded [`crate::model::module`] id, and a
//! host-function pointer → the resolved [`Function`] (module + entry). [`launch_kernel`] then resolves the
//! host pointer to its [`Function`], recovers the kernel's parameter layout from the module PTX (which
//! arguments are device pointers vs by-value scalars, and each width) via [`crate::adapter::ptx::compile`],
//! marshals each `args[i]` slot accordingly, and lowers through the SAME [`crate::service::launch::launch`]
//! the driver API uses — an identical `CreateShader{kernel}` + `CreateComputePipeline` + `CreateBindGroup`
//! + `Dispatch` sequence. There is no second launch code path.

use core::ffi::c_void;
use std::collections::HashMap;

use crate::adapter::ptx;
use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::model::module::{Function, KernelArg};
use crate::service::{launch, load_module};
use hl_gpu::{CommandSink, GpuError, Result};

/// An opaque fatbin-registration handle: what `__cudaRegisterFatBinary` hands nvcc's host stub, and what
/// `__cudaRegisterFunction` / `__cudaRegisterFatBinaryEnd` pass back. Its integer value keys the loaded
/// module in the [`Registry`]; it is otherwise opaque to the caller.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FatbinHandle(pub u64);

/// The process-global-ish runtime-API registry (the shim owns one instance behind its state mutex; a test
/// owns one directly). Two maps: fatbin handle → loaded module id, and host-function pointer → resolved
/// [`Function`].
#[derive(Debug, Default)]
pub struct Registry {
    /// `__cudaRegisterFatBinary` handle → the `hl_cuda` module id its PTX loaded as.
    modules: HashMap<u64, u32>,
    /// Host function pointer (as `usize`) → the device entry it was registered against.
    functions: HashMap<usize, Function>,
    /// Monotonic handle allocator (non-zero, so a zero handle is always invalid).
    next_handle: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
            functions: HashMap::new(),
            next_handle: 1,
        }
    }

    /// `__cudaRegisterFatBinary(fatbin)` — walk the fatbin container to its embedded PTX, load it as a
    /// module (via [`CudaContext::load_module`], which handles the fatbin/raw-PTX split), and return
    /// a fresh opaque handle bound to that module. `image` is the fatbin CONTAINER bytes (the shim follows
    /// the `__fatBinC_Wrapper_t` to them before calling in).
    pub fn register_fatbinary(
        &mut self,
        ctx: &mut CudaContext,
        image: &[u8],
    ) -> Result<FatbinHandle> {
        let module = ctx.load_module(image)?;
        let handle = self.next_handle;
        self.next_handle += 1;
        self.modules.insert(handle, module);
        hl_log::hl_debug!(
            hl_log::tag::CUDA,
            "register_fatbin handle={} mod={}",
            handle,
            module
        );
        hl_log::hl_count!(hl_log::tag::CUDA, "fatbins");
        Ok(FatbinHandle(handle))
    }

    /// `__cudaRegisterFunction(handle, hostFn, …, deviceName, …)` — resolve `device_name` to a
    /// [`Function`] in the handle's module and bind the host function pointer to it. Errors if the handle
    /// is unknown or the module has no such entry.
    pub fn register_function(
        &mut self,
        ctx: &CudaContext,
        handle: FatbinHandle,
        host_fn: usize,
        device_name: &str,
    ) -> Result<()> {
        let module = *self.modules.get(&handle.0).ok_or(GpuError::Invalid(
            "__cudaRegisterFunction: unknown fatbin handle",
        ))?;
        let func = load_module::module_get_function(ctx, module, device_name)?;
        self.functions.insert(host_fn, func);
        hl_log::hl_debug!(
            hl_log::tag::CUDA,
            "register_fn host={:#x} mod={} name={}",
            host_fn,
            module,
            device_name
        );
        Ok(())
    }

    /// `__cudaRegisterFatBinaryEnd(handle)` — the finalization marker nvcc emits after the last
    /// `__cudaRegisterFunction`. Registration is eager here (the module + functions are already live), so
    /// this only validates the handle is one we issued.
    pub fn register_fatbinary_end(&self, handle: FatbinHandle) -> bool {
        self.modules.contains_key(&handle.0)
    }

    /// `__cudaUnregisterFatBinary(handle)` — drop the handle's module binding (its functions stay resolved
    /// against the still-loaded module id; this only forgets the handle). Benign for an unknown handle.
    pub fn unregister_fatbinary(&mut self, handle: FatbinHandle) {
        self.modules.remove(&handle.0);
    }

    /// Resolve a host function pointer to the device [`Function`] it was registered against.
    pub fn resolve(&self, host_fn: usize) -> Option<Function> {
        self.functions.get(&host_fn).copied()
    }
}

/// `cudaLaunchKernel(hostFn, grid, block, args, sharedMem, stream)` — the runtime-API launch.
///
/// Resolve `host_fn` to its device [`Function`], recover the kernel's parameter layout from the module
/// PTX (pointer-vs-scalar classification + per-parameter width, via the same front-end the executor
/// compiles with), marshal each `args[i]` slot per that layout into a [`KernelArg`], and lower through
/// [`launch::launch`] — byte-for-byte the driver-API `cuLaunchKernel` compute sequence.
///
/// # Safety
/// `args` must be either null (only valid for a zero-parameter kernel) or a valid `void**` with at least
/// one readable slot per kernel parameter; each `args[i]` must point to a readable value of the i-th
/// parameter's width (the CUDA Runtime API's `void** args` contract).
pub unsafe fn launch_kernel(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    registry: &Registry,
    host_fn: usize,
    grid: (u32, u32, u32),
    block: (u32, u32, u32),
    args: *const *const c_void,
) -> Result<()> {
    let func = registry.resolve(host_fn).ok_or_else(|| {
        hl_log::hl_warn!(
            hl_log::tag::CUDA,
            "launch_kernel unregistered host={:#x}",
            host_fn
        );
        GpuError::Invalid("cudaLaunchKernel: host function not registered")
    })?;

    // Recover the parameter layout by compiling the module's PTX with the launch block dims — the exact
    // front-end the executor uses, so pointer/scalar classification + widths agree with the compiled kernel.
    let (src, entry) = ctx
        .entry_source(func)
        .ok_or(GpuError::Invalid("cudaLaunchKernel: stale function handle"))?;
    let prog = ptx::compile(&src, &entry, [block.0, block.1, block.2])?;

    if args.is_null() && !prog.params.is_empty() {
        // The `extra`-packed parameter form is not modeled; a parameterized kernel needs the `args` array.
        return Err(GpuError::Invalid(
            "cudaLaunchKernel: null args for a kernel with parameters",
        ));
    }

    // Marshal each argument out of its `args[i]` slot per the recovered layout.
    let mut kargs: Vec<KernelArg> = Vec::with_capacity(prog.params.len());
    for (i, p) in prog.params.iter().enumerate() {
        let slot = *args.add(i);
        if slot.is_null() {
            return Err(GpuError::Invalid("cudaLaunchKernel: null argument slot"));
        }
        if p.is_ptr {
            // A pointer parameter's slot holds the device address (u64).
            let v = std::ptr::read_unaligned(slot as *const u64);
            kargs.push(KernelArg::Ptr(DevicePtr(v)));
        } else {
            // A by-value scalar's slot holds `width` raw bytes.
            let w = p.width as usize;
            let raw = std::slice::from_raw_parts(slot as *const u8, w);
            kargs.push(KernelArg::Scalar(raw.to_vec()));
        }
    }

    launch::launch(ctx, sink, func, grid, block, &kargs)?;
    Ok(())
}
