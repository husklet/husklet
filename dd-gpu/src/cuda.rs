//! Simulated CUDA device + CUDA→dd-GPU-IR translation (the "show the container a CUDA device, run it
//! on host Metal" path). See `docs/ideas/CUDA_ON_METAL.md` for the full design and the honest
//! feasibility phasing.
//!
//! Layers, matching the doc's phases:
//!   * **Device presence** ([`CudaDeviceDesc`]) — the numbers NVML / `nvidia-smi` / the CUDA runtime
//!     report so `torch.cuda.is_available()` and driver probes succeed. Pure data; fully testable here.
//!   * **Driver-API translation** ([`CudaContext`]) — `cuMemAlloc`→[`Cmd::CreateBuffer`],
//!     `cuMemcpyHtoD`→[`Cmd::WriteBuffer`], `cuMemcpyDtoH`→backend readback, `cuLaunchKernel`→a compute
//!     [`Cmd::Submit`]. Emits the exact IR a Metal executor replays; testable against the software
//!     backend for the memory path.
//!   * **PTX** ([`PtxModule`]) — we parse kernel entry points out of PTX today (real, testable); the
//!     PTX→AIR/MSL *body* translation is the research-grade core, done host-side and out of scope here.

use crate::id::BufferId;
use crate::ir::*;
use crate::ptx::KernelDescriptor;
use std::collections::HashMap;

/// What NVML / `nvidia-smi` / `cudaGetDeviceProperties` report for the simulated device. The values are
/// chosen to look like a plausible NVIDIA GPU so unmodified probes accept it, while actually being
/// served by the host Metal GPU.
#[derive(Clone, PartialEq, Debug)]
pub struct CudaDeviceDesc {
    pub name: String,
    /// (major, minor) compute capability, e.g. (8, 6) ~ Ampere.
    pub compute_capability: (u32, u32),
    /// Reported VRAM in bytes (on Apple Silicon this is carved from unified memory).
    pub total_mem: u64,
    pub multiprocessor_count: u32,
    pub warp_size: u32,
    pub max_threads_per_block: u32,
    pub clock_khz: u32,
    /// 16-byte GPU UUID reported by NVML (`GPU-xxxxxxxx-....`).
    pub uuid: [u8; 16],
    pub pci_bus_id: String,
}

impl CudaDeviceDesc {
    /// A sensible default for an Apple-silicon host: presents as a mid-range Ampere-class device backed
    /// by unified memory. `vram_bytes` should be a slice of the machine's RAM the user allows.
    pub fn apple_default(vram_bytes: u64) -> Self {
        Self {
            name: "dd Metal (CUDA-sim) Device".into(),
            compute_capability: (8, 6),
            total_mem: vram_bytes,
            multiprocessor_count: 32,
            warp_size: 32,
            max_threads_per_block: 1024,
            clock_khz: 1_500_000,
            uuid: *b"dd-metal-cuda\0\0\0",
            pci_bus_id: "0000:00:00.0".into(),
        }
    }

    pub fn compute_capability_str(&self) -> String {
        format!("{}.{}", self.compute_capability.0, self.compute_capability.1)
    }

    /// A UUID in NVML's textual `GPU-...` form.
    pub fn uuid_str(&self) -> String {
        let h: String = self.uuid.iter().map(|b| format!("{:02x}", b)).collect();
        format!(
            "GPU-{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    }

    /// The line `nvidia-smi -L` would print for this device (index `idx`).
    pub fn nvidia_smi_l_line(&self, idx: u32) -> String {
        format!("GPU {}: {} (UUID: {})", idx, self.name, self.uuid_str())
    }
}

/// A CUDA device pointer (opaque `CUdeviceptr` value handed back to the guest).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct DevicePtr(pub u64);

/// A loaded PTX module. We can extract entry-point names without translating the body — enough to
/// implement `cuModuleGetFunction` and to know how many kernels a module carries.
#[derive(Clone, PartialEq, Debug)]
pub struct PtxModule {
    pub source: String,
    pub entries: Vec<String>,
}

impl PtxModule {
    /// Parse `.entry` / `.visible .entry` kernel names out of PTX text. Real and testable; does NOT
    /// translate kernel bodies (that is the PTX→AIR/MSL research-grade step, host-side, out of scope).
    pub fn parse(source: &str) -> Self {
        let mut entries = Vec::new();
        for line in source.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix(".entry").map(str::trim).or_else(|| {
                l.strip_prefix(".visible")
                    .map(str::trim)
                    .and_then(|r| r.strip_prefix(".entry"))
                    .map(str::trim)
            }) {
                // name is up to '(' or whitespace
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                if !name.is_empty() {
                    entries.push(name);
                }
            }
        }
        Self { source: source.to_string(), entries }
    }
}

/// A kernel argument in a `cuLaunchKernel` call.
#[derive(Clone, PartialEq, Debug)]
pub enum KernelArg {
    /// A device-pointer argument → bound as a storage buffer.
    Ptr(DevicePtr),
    /// A by-value scalar → packed into a uniform buffer of arguments.
    Scalar(Vec<u8>),
}

struct Alloc {
    buffer: u32,
    size: u64,
}

/// A resolved CUDA function handle (module + entry index).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Function {
    pub module: u32,
    pub entry: u32,
}

/// Translates a stream of intercepted CUDA driver-API calls into dd-GPU IR + tracks device state.
///
/// Backends replay the emitted [`Cmd`]s; readbacks (`cuMemcpyDtoH`) are resolved by the caller via
/// [`crate::backend::GpuBackend::read_buffer`] using [`CudaContext::resolve`].
pub struct CudaContext {
    pub device: CudaDeviceDesc,
    next_ptr: u64,
    next_buffer: u32,
    next_module: u32,
    next_shader: u32,
    next_pipeline: u32,
    next_bind_group: u32,
    allocs: HashMap<u64, Alloc>, // base ptr -> allocation
    modules: HashMap<u32, PtxModule>,
    /// Cache: (module, entry, block) → (shader id, pipeline id) so a repeated launch reuses the
    /// pipeline. The block dims are part of the key because they are baked into the compiled kernel
    /// (the WebGPU/Metal `local_size`), exactly as the design mandates.
    pipelines: HashMap<(u32, u32, [u32; 3]), (u32, u32)>,
}

impl CudaContext {
    pub fn new(device: CudaDeviceDesc) -> Self {
        Self {
            device,
            // start device pointers well above 0 and page-aligned, like a real allocator
            next_ptr: 0x10_0000,
            next_buffer: 1,
            next_module: 1,
            next_shader: 1,
            next_pipeline: 1,
            next_bind_group: 1,
            allocs: HashMap::new(),
            modules: HashMap::new(),
            pipelines: HashMap::new(),
        }
    }

    fn align_up(v: u64, a: u64) -> u64 {
        (v + a - 1) & !(a - 1)
    }

    /// `cuMemAlloc(size)` → a device pointer + the IR to create the backing buffer.
    pub fn mem_alloc(&mut self, size: u64) -> (DevicePtr, Cmd) {
        let ptr = self.next_ptr;
        // bump with 256-byte alignment (CUDA guarantees ≥256B alignment)
        self.next_ptr = Self::align_up(self.next_ptr + size.max(1), 256);
        let buffer = self.next_buffer;
        self.next_buffer += 1;
        self.allocs.insert(ptr, Alloc { buffer, size });
        let cmd = Cmd::CreateBuffer(
            buffer,
            BufferDesc {
                size,
                usage: buffer_usage::STORAGE
                    | buffer_usage::COPY_SRC
                    | buffer_usage::COPY_DST
                    | buffer_usage::MAP,
                label: String::new(),
            },
        );
        (DevicePtr(ptr), cmd)
    }

    /// Map a (possibly offset) device pointer back to (buffer id, byte offset). Returns `None` for a
    /// dangling pointer — the translation-layer equivalent of `CUDA_ERROR_INVALID_VALUE`.
    pub fn resolve(&self, p: DevicePtr) -> Option<(BufferId, u64)> {
        // find the allocation whose [base, base+size) contains p
        for (&base, a) in &self.allocs {
            if p.0 >= base && p.0 < base + a.size.max(1) {
                return Some((BufferId(a.buffer), p.0 - base));
            }
        }
        None
    }

    /// `cuMemFree(ptr)` → destroy the backing buffer.
    pub fn mem_free(&mut self, p: DevicePtr) -> Option<Cmd> {
        let a = self.allocs.remove(&p.0)?;
        Some(Cmd::DestroyBuffer(a.buffer))
    }

    /// `cuMemcpyHtoD(dst, src, n)` → write into the backing buffer at the right offset.
    pub fn memcpy_htod(&self, dst: DevicePtr, src: &[u8]) -> Option<Cmd> {
        let (buf, off) = self.resolve(dst)?;
        Some(Cmd::WriteBuffer {
            id: buf.0,
            offset: off,
            data: src.to_vec(),
        })
    }

    /// `cuModuleLoadData(ptx)` → module id (entry points parsed now; body translated host-side later).
    pub fn module_load(&mut self, ptx: &str) -> u32 {
        let id = self.next_module;
        self.next_module += 1;
        self.modules.insert(id, PtxModule::parse(ptx));
        id
    }

    pub fn module(&self, id: u32) -> Option<&PtxModule> {
        self.modules.get(&id)
    }

    /// `cuModuleGetFunction(module, name)`.
    pub fn module_get_function(&self, module: u32, name: &str) -> Option<Function> {
        let m = self.modules.get(&module)?;
        let entry = m.entries.iter().position(|e| e == name)? as u32;
        Some(Function { module, entry })
    }

    /// `cuLaunchKernel(func, grid, block, args)` → the compute IR: (lazily) a kernel shader + compute
    /// pipeline, a flat parameter buffer + storage bindings over the args, and a one-dispatch command
    /// buffer that a [`crate::backend::GpuBackend`] replays.
    ///
    /// `grid` = number of blocks (→ workgroup count / `dispatchThreadgroups`); `block` = threads per
    /// block (→ threadgroup size, baked into the compiled kernel as the WebGPU/Metal `local_size`).
    ///
    /// ## Kernel argument ABI (the seam the interpreter/Metal both honor)
    /// * **binding 0** = the flat kernel-parameter blob, laid out exactly like CUDA's `cuLaunchKernel`
    ///   parameter space (each argument at its natural-aligned offset). Scalar params are read from here.
    /// * **binding r+1** = the storage buffer for the `r`-th pointer argument (in argument order). A
    ///   pointer parameter dereferences its own region — the per-allocation binding model Metal uses.
    ///
    /// Returns the IR plus the pipeline id used (repeat launches of the same `(module, entry, block)`
    /// reuse the cached shader+pipeline and emit no new `CreateShader`/`CreateComputePipeline`).
    pub fn launch(
        &mut self,
        func: Function,
        grid: (u32, u32, u32),
        block: (u32, u32, u32),
        args: &[KernelArg],
    ) -> Vec<Cmd> {
        let mut out = Vec::new();
        let block_arr = [block.0, block.1, block.2];

        // lazily create the kernel shader (forwarded PTX descriptor) + compute pipeline.
        let pipeline = if let Some(&(_, pipeline)) =
            self.pipelines.get(&(func.module, func.entry, block_arr))
        {
            pipeline
        } else {
            let shader = self.next_shader;
            self.next_shader += 1;
            let pipeline = self.next_pipeline;
            self.next_pipeline += 1;
            // The "shader" carried across the dd-GPU IR is a kernel descriptor: the module's PTX text +
            // the entry name + the launch block dims. The host backend compiles it (software → dd-GPU
            // kernel IR + CPU interpreter; a Metal host → PTX→SPIR-V→MSL). See CUDA_ON_METAL.md §5.
            let (ptx_src, entry_name) = self
                .modules
                .get(&func.module)
                .and_then(|m| {
                    m.entries
                        .get(func.entry as usize)
                        .map(|e| (m.source.clone(), e.clone()))
                })
                .unwrap_or_default();
            let desc = KernelDescriptor { ptx: ptx_src, entry: entry_name.clone(), block: block_arr };
            out.push(Cmd::CreateShader { id: shader, spirv: desc.to_words() });
            out.push(Cmd::CreateComputePipeline(
                pipeline,
                ComputePipelineDesc {
                    compute: ShaderRef { module: shader, entry: entry_name },
                    label: String::new(),
                },
            ));
            self.pipelines.insert((func.module, func.entry, block_arr), (shader, pipeline));
            pipeline
        };

        // Build the flat parameter blob (binding 0) and the pointer storage bindings (binding r+1).
        let mut blob: Vec<u8> = Vec::new();
        let mut entries: Vec<BindEntry> = Vec::new();
        let mut region = 0u32;
        for a in args {
            match a {
                KernelArg::Ptr(p) => {
                    // natural-align to 8 (pointer width), then store the device address in the blob.
                    align_blob(&mut blob, 8);
                    blob.extend_from_slice(&p.0.to_le_bytes());
                    if let Some((buf, off)) = self.resolve(*p) {
                        entries.push(BindEntry {
                            binding: region + 1,
                            resource: BindResource::Buffer { id: buf.0, offset: off, size: 0 },
                        });
                    }
                    region += 1;
                }
                KernelArg::Scalar(bytes) => {
                    align_blob(&mut blob, bytes.len().max(1) as u64);
                    blob.extend_from_slice(bytes);
                }
            }
        }

        // Materialize the parameter blob as a small uniform/storage buffer bound at binding 0.
        let param_buf = self.next_buffer;
        self.next_buffer += 1;
        out.push(Cmd::CreateBuffer(
            param_buf,
            BufferDesc {
                size: blob.len().max(1) as u64,
                usage: buffer_usage::UNIFORM | buffer_usage::STORAGE | buffer_usage::COPY_DST,
                label: "kernel-params".into(),
            },
        ));
        if !blob.is_empty() {
            out.push(Cmd::WriteBuffer { id: param_buf, offset: 0, data: blob });
        }
        entries.insert(
            0,
            BindEntry { binding: 0, resource: BindResource::Buffer { id: param_buf, offset: 0, size: 0 } },
        );

        let bind_group = self.next_bind_group;
        self.next_bind_group += 1;
        out.push(Cmd::CreateBindGroup(bind_group, BindGroupDesc { set: 0, entries }));

        out.push(Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(pipeline),
                Enc::SetBindGroup { index: 0, group: bind_group },
                Enc::Dispatch { x: grid.0, y: grid.1, z: grid.2 },
                Enc::EndComputePass,
            ],
            signal: None,
        }));
        // The parameter buffer and bind group are launch-local: the submit above is synchronous
        // (the executor completes the dispatch before returning), so release them right after so
        // repeated launches don't grow the backend's resource tables without bound. The cached
        // shader/pipeline (keyed by entry+block) intentionally persist for reuse.
        out.push(Cmd::DestroyBindGroup(bind_group));
        out.push(Cmd::DestroyBuffer(param_buf));
        out
    }
}

/// Pad `blob` up to a natural-alignment boundary before appending the next kernel parameter.
fn align_blob(blob: &mut Vec<u8>, align: u64) {
    let a = align as usize;
    while blob.len() % a != 0 {
        blob.push(0);
    }
}
