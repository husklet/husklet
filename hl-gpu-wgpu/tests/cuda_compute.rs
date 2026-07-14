//! CUDA compute on the REAL Metal GPU through wgpu.
//!
//! The milestone this proves: a real CUDA PTX kernel (vector-add, `c[i] = a[i] + b[i]` — the exact
//! kernel `hl-shim-cuda` / the software oracle run on the CPU) executes end-to-end on a live Metal
//! device via the wgpu backend, and the readback matches `a + b`. The path is
//! `PTX → hl-GPU kernel IR → WGSL → naga → MSL → wgpu::ComputePipeline → dispatch → storage buffer readback`.
//!
//! Needs a Metal device, so it only runs on macOS (empty elsewhere). Run on the mac:
//! `cargo test -p hl-gpu-wgpu --test cuda_compute`.

#![cfg(target_os = "macos")]

use hl_gpu::cuda::{CudaContext, CudaDeviceDesc, KernelArg};
use hl_gpu::replay;
use hl_gpu_wgpu::WgpuBackend;

/// Generate the CUDA vecadd command stream via the CUDA→hl-GPU-IR translation, replay it through the
/// real wgpu/Metal backend, and assert the device readback equals `a + b` element-wise.
#[test]
fn cuda_vecadd_runs_on_real_metal() {
    let n = 1024usize;
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(4 << 30));
    let mut be = WgpuBackend::new().expect("wgpu Metal backend");

    // cuMemAlloc a, b, c
    let (a, ca) = ctx.mem_alloc((n * 4) as u64);
    let (b, cb) = ctx.mem_alloc((n * 4) as u64);
    let (c, cc) = ctx.mem_alloc((n * 4) as u64);
    for cmd in [&ca, &cb, &cc] {
        replay::apply(&mut be, cmd).expect("alloc");
    }

    // host inputs → cuMemcpyHtoD
    let ha: Vec<f32> = (0..n).map(|i| i as f32).collect();
    let hb: Vec<f32> = (0..n).map(|i| (n - i) as f32 * 0.5).collect();
    let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    replay::apply(&mut be, &ctx.memcpy_htod(a, &bytes(&ha)).unwrap()).expect("h2d a");
    replay::apply(&mut be, &ctx.memcpy_htod(b, &bytes(&hb)).unwrap()).expect("h2d b");

    // cuModuleLoadData(vecadd PTX) + cuModuleGetFunction + cuLaunchKernel<<<ceil(n/256), 256>>>(a,b,c,n)
    let m = ctx.module_load(hl_gpu::ptx::VECADD_PTX);
    let f = ctx.module_get_function(m, "vecadd").expect("entry vecadd");
    let block = (256u32, 1, 1);
    let grid = ((n as u32).div_ceil(256), 1, 1);
    let args = [
        KernelArg::Ptr(a),
        KernelArg::Ptr(b),
        KernelArg::Ptr(c),
        KernelArg::Scalar((n as u32).to_le_bytes().to_vec()),
    ];
    for cmd in &ctx.launch(f, grid, block, &args) {
        replay::apply(&mut be, cmd).expect("launch cmd");
    }

    // cuMemcpyDtoH(c) → assert c[i] == a[i] + b[i]
    let (cbuf, coff) = ctx.resolve(c).unwrap();
    let mut out = vec![0u8; n * 4];
    use hl_gpu::backend::GpuBackend;
    be.read_buffer(cbuf, coff, &mut out).expect("d2h c");
    let mut mism = 0;
    for i in 0..n {
        let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
        let want = ha[i] + hb[i];
        if got != want {
            if mism < 8 {
                eprintln!("mismatch c[{i}]: got {got} want {want}");
            }
            mism += 1;
        }
    }
    assert_eq!(mism, 0, "vecadd on real Metal: {mism} mismatched elements");
    // spot value the report can quote: c[3] = 3 + (1024-3)*0.5 = 3 + 510.5 = 513.5
    let c3 = f32::from_le_bytes(out[12..16].try_into().unwrap());
    assert_eq!(c3, 513.5);
    eprintln!("cuda_vecadd_runs_on_real_metal: OK (n={n}, c[3]={c3})");
}
