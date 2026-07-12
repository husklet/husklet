//! CUDA block reduction (shared memory + bar.sync tree reduction + a final global atomicAdd) on the
//! REAL Metal GPU through wgpu — and cross-checked against the CPU `SoftwareBackend` oracle.
//!
//! This is the milestone for the shared-memory + barriers + atomics PTX subset: the same `block_reduce`
//! PTX kernel runs both on a live Metal device (`PTX → dd-GPU kernel IR → WGSL → naga → MSL →
//! wgpu::ComputePipeline`) and on the CPU interpreter (`dd_gpu::software::SoftwareBackend`), and BOTH
//! produce the same, arithmetically-correct sum. The Metal path proves `var<workgroup>` shared memory,
//! `workgroupBarrier()`, and `atomicAdd` on a storage buffer all execute correctly end-to-end.
//!
//! Needs a Metal device, so macOS-only. Run on the mac:
//! `cargo test -p dd-gpu-wgpu --test cuda_reduce`.

#![cfg(target_os = "macos")]

use dd_gpu::backend::GpuBackend;
use dd_gpu::cuda::{CudaContext, CudaDeviceDesc, KernelArg};
use dd_gpu::replay;

/// Build the `block_reduce` CUDA command stream (alloc in/out, H2D input + zeroed accumulator, launch
/// `ceil(n/256)` blocks of 256), replay it on `be`, and return the single-int accumulator read back.
fn run_reduce<B: GpuBackend>(be: &mut B, data: &[i32]) -> i32 {
    let n = data.len();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(4 << 30));

    let (din, cin) = ctx.mem_alloc((n * 4).max(4) as u64);
    let (dout, cout) = ctx.mem_alloc(4);
    for cmd in [&cin, &cout] {
        replay::apply(be, cmd).expect("alloc");
    }

    let in_bytes: Vec<u8> = data.iter().flat_map(|x| x.to_le_bytes()).collect();
    replay::apply(be, &ctx.memcpy_htod(din, &in_bytes).unwrap()).expect("h2d in");
    // zero the accumulator (atomicAdd accumulates into it).
    replay::apply(be, &ctx.memcpy_htod(dout, &0i32.to_le_bytes()).unwrap()).expect("h2d out");

    let m = ctx.module_load(dd_gpu::ptx::REDUCE_PTX);
    let f = ctx.module_get_function(m, "block_reduce").expect("entry block_reduce");
    let block = (256u32, 1, 1);
    let grid = ((n as u32).div_ceil(256).max(1), 1, 1);
    let args = [
        KernelArg::Ptr(din),
        KernelArg::Ptr(dout),
        KernelArg::Scalar((n as u32).to_le_bytes().to_vec()),
    ];
    for cmd in &ctx.launch(f, grid, block, &args) {
        replay::apply(be, cmd).expect("launch cmd");
    }

    let (obuf, ooff) = ctx.resolve(dout).unwrap();
    let mut out = [0u8; 4];
    be.read_buffer(obuf, ooff, &mut out).expect("d2h out");
    i32::from_le_bytes(out)
}

#[test]
fn cuda_block_reduce_runs_on_real_metal() {
    use dd_gpu::software::SoftwareBackend;
    use dd_gpu_wgpu::WgpuBackend;

    for &n in &[1usize, 255, 256, 257, 1000, 4096, 5000] {
        let data: Vec<i32> = (0..n as i32).map(|i| (i % 17) - 8).collect();
        let want: i32 = data.iter().sum();

        // CPU oracle (the software interpreter backend).
        let mut soft = SoftwareBackend::new();
        let cpu = run_reduce(&mut soft, &data);

        // Real Metal GPU via wgpu.
        let mut gpu = WgpuBackend::new().expect("wgpu Metal backend");
        let metal = run_reduce(&mut gpu, &data);

        assert_eq!(cpu, want, "software oracle sum mismatch at n={n}");
        assert_eq!(metal, want, "real-Metal reduction sum mismatch at n={n}: got {metal} want {want}");
        assert_eq!(metal, cpu, "Metal vs CPU-oracle disagree at n={n}");
        eprintln!("cuda_block_reduce n={n}: metal={metal} cpu={cpu} want={want} OK");
    }
}
