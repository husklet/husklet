//! Vulkan-style compute on the REAL Metal GPU through wgpu.
//!
//! The milestone this proves: a **SPIR-V** compute shader (the shader format a Vulkan app hands the
//! driver — here a `c[i] = a[i] + b[i]` vector-add) executes end to end on a live Metal device via the
//! wgpu backend, and the readback matches `a + b`. Unlike `cuda_compute.rs` (which sources the kernel
//! from CUDA PTX via the `KERNEL_MAGIC` descriptor seam), this drives the backend's *raw SPIR-V* path:
//!   `SPIR-V words → CreateShader{spirv} → naga::front::spv → WGSL → naga → MSL → wgpu::ComputePipeline
//!    → dispatch → storage-buffer readback`.
//! This is the exact host execution seam the guest `dd-shim-vk` (Vulkan ICD) will target.
//!
//! The SPIR-V is minted on the host from a GLSL 450 (Vulkan dialect) source via naga's GLSL front end +
//! SPIR-V back end — i.e. the same GLSL→SPIR-V step a Vulkan toolchain (glslang) performs — with a WGSL
//! source as a fallback so the numeric proof holds regardless of the GLSL front end's SSBO coverage.
//! Either way the bytes fed to `CreateShader` carry the real SPIR-V magic and flow through
//! `naga::front::spv`. Needs a Metal device, so macOS only. Run: `cargo test -p dd-gpu-wgpu --test spirv_compute`.
//!
//! Semantics grounded in MoltenVK (the canonical Vulkan-over-Metal driver), not guessed:
//!   * SPIR-V→MSL — `MoltenVKShaderConverter/SPIRVToMSLConverter.cpp`: the entry point is selected by
//!     (name, stage) via SPIRV-Cross `set_entry_point` (l.282) and the MTLFunction is looked up by that
//!     name (`populateEntryPoint`, l.543) — so the pipeline names its entry "main", exactly as here.
//!     `MSLResourceBinding` (l.108-336) maps each (descriptorSet, binding) to an MSL buffer index; the
//!     compute workgroup size is read from the SPIR-V entry point in `getWorkgroupSize` (l.504). naga+wgpu
//!     do the equivalent: the bind-group layout and `@workgroup_size` are derived from the SPIR-V, so our
//!     `set=0` bindings 0/1/2 and the shader's `local_size_x=64` need no manual buffer-index/threadgroup map.
//!   * Dispatch — `Commands/MVKCmdDispatch.mm` encodes `dispatchThreadgroups: count
//!     threadsPerThreadgroup: pipeline->getThreadgroupSize()` (l.11-12). Our `Enc::Dispatch{x=n/64}` →
//!     `wgpu compute_pass.dispatch_workgroups`, with the threadgroup size supplied by the compiled kernel —
//!     the same split of "group count from the command, threads-per-group from the shader".

#![cfg(target_os = "macos")]

use dd_gpu::backend::GpuBackend;
use dd_gpu::id::BufferId;
use dd_gpu::ir::*;
use dd_gpu_wgpu::WgpuBackend;

/// Validate a naga module and emit Vulkan SPIR-V words (default `spv` back end targets Vulkan).
fn module_to_spirv(module: &naga::Module) -> Result<Vec<u32>, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("validate: {e:?}"))?;
    naga::back::spv::write_vec(module, &info, &naga::back::spv::Options::default(), None)
        .map_err(|e| format!("spv-out: {e:?}"))
}

/// GLSL 450 (Vulkan dialect) → SPIR-V, the authentic Vulkan compile step (as glslang would do).
fn glsl_to_spirv(src: &str, stage: naga::ShaderStage) -> Result<Vec<u32>, String> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(stage), src)
        .map_err(|e| format!("glsl-in: {e:?}"))?;
    module_to_spirv(&module)
}

/// WGSL → SPIR-V, the codebase's established SPIR-V-minting fallback (see `shader.rs` tests).
fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("wgsl parse");
    module_to_spirv(&module).expect("wgsl→spv")
}

const VECADD_GLSL: &str = "\
#version 450
layout(local_size_x = 64) in;
// Plain (read_write) SSBOs: naga's GLSL front end doesn't model `readonly`/`writeonly` storage
// access qualifiers, so the Vulkan-authentic path uses unqualified buffers.
layout(set = 0, binding = 0) buffer A { float a[]; };
layout(set = 0, binding = 1) buffer B { float b[]; };
layout(set = 0, binding = 2) buffer C { float c[]; };
void main() {
    uint i = gl_GlobalInvocationID.x;
    c[i] = a[i] + b[i];
}
";

const VECADD_WGSL: &str = "\
@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    c[i] = a[i] + b[i];
}
";

/// Mint the vecadd compute SPIR-V, preferring the GLSL (Vulkan) front end; fall back to WGSL if naga's
/// GLSL front end can't handle the runtime-sized SSBO arrays. Prints which source produced the bytes.
fn vecadd_spirv() -> Vec<u32> {
    match glsl_to_spirv(VECADD_GLSL, naga::ShaderStage::Compute) {
        Ok(words) => {
            eprintln!("spirv_compute: SPIR-V minted from GLSL 450 (Vulkan path)");
            words
        }
        Err(e) => {
            eprintln!("spirv_compute: GLSL front end unavailable for SSBO ({e}); using WGSL-sourced SPIR-V");
            wgsl_to_spirv(VECADD_WGSL)
        }
    }
}

#[test]
fn spirv_vecadd_runs_on_real_metal() {
    const N: usize = 1024;
    let words = vecadd_spirv();
    assert_eq!(words.first().copied(), Some(0x0723_0203), "payload is real SPIR-V");

    let mut be = WgpuBackend::new().expect("wgpu Metal backend");

    let ha: Vec<f32> = (0..N).map(|i| i as f32).collect();
    let hb: Vec<f32> = (0..N).map(|i| (N - i) as f32 * 0.5).collect();
    let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();

    // Buffer ids: 1=a 2=b 3=c. STORAGE for the compute bindings; the backend always adds COPY_SRC/DST
    // so the DtoH readback and HtoD upload work without extra usage bits.
    let store = buffer_usage::STORAGE;
    let cmds = vec![
        Cmd::CreateBuffer(1, BufferDesc { size: (N * 4) as u64, usage: store, label: "a".into() }),
        Cmd::CreateBuffer(2, BufferDesc { size: (N * 4) as u64, usage: store, label: "b".into() }),
        Cmd::CreateBuffer(3, BufferDesc { size: (N * 4) as u64, usage: store, label: "c".into() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: bytes(&ha) },
        Cmd::WriteBuffer { id: 2, offset: 0, data: bytes(&hb) },
        Cmd::CreateShader { id: 20, kind: dd_gpu::ir::ShaderPayloadKind::SpirV, spirv: words },
        Cmd::CreateComputePipeline(30, ComputePipelineDesc {
            compute: ShaderRef { module: 20, entry: "main".into() },
            label: "vecadd".into(),
        }),
        Cmd::CreateBindGroup(40, BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: (N * 4) as u64 } },
                BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: (N * 4) as u64 } },
                BindEntry { binding: 2, resource: BindResource::Buffer { id: 3, offset: 0, size: (N * 4) as u64 } },
            ],
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(30),
                Enc::SetBindGroup { index: 0, group: 40 },
                Enc::Dispatch { x: (N as u32).div_ceil(64), y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ];
    let ir = encode_stream(&cmds);
    dd_gpu::replay::replay_stream(&mut be, &ir).expect("replay spirv compute");

    let mut out = vec![0u8; N * 4];
    be.read_buffer(BufferId(3), 0, &mut out).expect("DtoH c");
    let mut mism = 0;
    for i in 0..N {
        let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
        let want = ha[i] + hb[i];
        if got != want {
            if mism < 8 {
                eprintln!("mismatch c[{i}]: got {got} want {want}");
            }
            mism += 1;
        }
    }
    assert_eq!(mism, 0, "SPIR-V vecadd on real Metal: {mism} mismatched elements");
    // spot value the report can quote: c[3] = 3 + (1024-3)*0.5 = 513.5
    let c3 = f32::from_le_bytes(out[12..16].try_into().unwrap());
    assert_eq!(c3, 513.5);
    eprintln!("spirv_vecadd_runs_on_real_metal: OK (n={N}, c[3]={c3})");
}
