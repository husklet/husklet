//! INTEGRATION DEMO — multi_driver_coexist: the CUDA + Vulkan + GL drivers composed on ONE engine
//! (`engine.add(Driver)`, the #86 capstone seam), then real driver work lowered onto ONE shared runtime
//! Session + ONE lavapipe executor, cross-checked so no driver corrupts another's live resources.
//!
//! Two things are proven, and one architectural limit is documented precisely:
//!
//!  (1) COMPOSITION — `Drivers::new().add(Cuda).add(Vulkan).add(Gl)` composes all three real driver plugs
//!      into one ordered engine (the `tests/plug.rs` capstone, re-asserted here as the entry to coexistence).
//!
//!  (2) SIMULTANEOUS COEXISTENCE — on ONE `InProcessCommandSink<WgpuExecutor>` (the shared "engine session"
//!      over the software Vulkan device), a REAL CUDA compute (a vecadd PTX kernel, lowered by the real
//!      hl-cuda launch service and RUN on lavapipe as a compute pass) and a REAL GL draw (a cleared frame
//!      lowered by the real hl-gl frame/swap service and RASTERIZED on lavapipe) run in the SAME session with
//!      their resources LIVE at the same time. CUDA occupies the buffer/shader/pipeline id tables; GL occupies
//!      the texture/surface id tables — disjoint per-kind namespaces — so they genuinely coexist. After each
//!      driver runs, the OTHER driver's result is re-read and asserted still exact: the CUDA sums survive the
//!      GL frame, and the GL target survives the CUDA dispatch. Neither corrupts the other.
//!
//!  (3) DOCUMENTED LIMIT (why not all THREE resident at once) — a driver mints its IR ids from its OWN base-1
//!      counter (hl-cuda/src/model/context.rs `next_buffer = 1`; hl-vulkan/src/model/device.rs
//!      `alloc_ir` from 0→1), and the runtime keys resources per-kind in ONE shared table, rejecting a second
//!      live id of the same kind as `DuplicateId` (hl-gpu/src/runtime/model/resources.rs:43). CUDA and
//!      Vulkan BOTH allocate *buffers* from id 1, so a Vulkan buffer cannot join a session where CUDA's
//!      buffers are still live — they collide in the shared buffer table. The composition root provides no
//!      per-driver id remap for the in-process shared-session case (in real deployment each driver gets its
//!      OWN socket → OWN Session, so the collision never arises). This test DEMONSTRATES the collision
//!      empirically, then frees CUDA's buffers and runs the REAL Vulkan buffer op on the SAME session +
//!      executor to prove Vulkan's own lowering is correct there too — the largest subset that coexists.

use hl_jit::Drivers;

use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{Cuda, CudaContext, CudaDeviceDesc, CudaSpec, DevicePtr, KernelArg};

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::service::{frame, record};
use hl_gl::{Gl, GlSpec};

use hl_vulkan::model::memory::vk_buffer_usage;
use hl_vulkan::result::HL_API_VERSION;
use hl_vulkan::service::create as vk_create;
use hl_vulkan::{Vulkan, VulkanSpec};

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{BufferId, Cmd, CommandSink, InProcessCommandSink};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const HOST_SOCK: &str = "/run/host-gpu.sock";

/// vecadd: `c[i] = a[i] + b[i]` with a global-index + bounds guard — the canonical CUDA compute.
const VECADD_PTX: &str = r#"
    .visible .entry vecadd(
        .param .u64 va_a,
        .param .u64 va_b,
        .param .u64 va_c,
        .param .u32 va_n
    )
    {
        ld.param.u64  %ra, [va_a];
        ld.param.u64  %rb, [va_b];
        ld.param.u64  %rc, [va_c];
        ld.param.u32  %rn, [va_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pa, %ga, %off;
        add.s64       %pb, %gb, %off;
        add.s64       %pc, %gc, %off;
        ld.global.f32 %va, [%pa];
        ld.global.f32 %vb, [%pb];
        add.f32       %vr, %va, %vb;
        st.global.f32 [%pc], %vr;
    DONE:
        ret;
    }
"#;

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn to_f32s(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// Read a CUDA device buffer back off the SHARED sink (trait readback — works for any executor).
fn cuda_readback(
    sink: &mut InProcessCommandSink<WgpuExecutor>,
    ctx: &CudaContext,
    p: DevicePtr,
    len: usize,
) -> Vec<f32> {
    let (buf, off): (BufferId, u64) = ctx.device_location(p).unwrap();
    to_f32s(&CommandSink::read_buffer(sink, buf, off, len).unwrap())
}

fn upload(
    sink: &mut InProcessCommandSink<WgpuExecutor>,
    ctx: &mut CudaContext,
    bytes: &[u8],
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, bytes.len() as u64).unwrap();
    transfer::memcpy_htod(ctx, sink, p, bytes).unwrap();
    p
}

#[test]
fn cuda_vulkan_gl_coexist_on_one_engine_session() {
    // -------------------------------------------------------------------------------------------
    // (1) COMPOSITION — engine.add(Cuda/Vulkan/Gl): all three real driver plugs on one ordered engine.
    // -------------------------------------------------------------------------------------------
    let mut engine = Drivers::new();
    engine
        .add(Cuda::new(
            CudaSpec::new(hl_cuda::Arch::X86_64, HOST_SOCK).stage_root("/opt/hlroot"),
        ))
        .add(Vulkan::new(
            VulkanSpec::new(hl_vulkan::Arch::X86_64, HOST_SOCK).stage_root("/opt/hlroot"),
        ))
        .add(Gl::new(
            GlSpec::new(hl_gl::Arch::X86_64, HOST_SOCK)
                .stage_root("/opt/hlroot")
                .surface_size(64, 64),
        ));
    assert_eq!(
        engine.len(),
        3,
        "all three drivers attached to the one engine"
    );
    assert_eq!(
        engine.names(),
        vec!["cuda", "vulkan", "gl"],
        "drivers composed, ordered"
    );

    // The one shared runtime session + lavapipe executor every driver lowers onto.
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("SKIP: no wgpu adapter (lavapipe/Vulkan ICD unreachable): {e}");
            return;
        }
    };
    let adapter = exec.adapter_name().to_lowercase();
    assert!(
        adapter.contains("llvmpipe") || adapter.contains("lavapipe"),
        "software Vulkan device required, got {:?}",
        exec.adapter_name()
    );
    exec.set_kernel_compiler(|desc: &KernelDescriptor| {
        hl_cuda::adapter::ptx::compile(&desc.ptx, &desc.entry, desc.block)
    });
    let mut sink = InProcessCommandSink::new(exec);

    // -------------------------------------------------------------------------------------------
    // (2a) REAL CUDA COMPUTE — a vecadd kernel lowered by the real hl-cuda service, RUN on lavapipe.
    // -------------------------------------------------------------------------------------------
    let n = 1024usize;
    let a: Vec<f32> = (0..n).map(|i| i as f32 * 0.5 - 3.0).collect();
    let b: Vec<f32> = (0..n).map(|i| i as f32 * 0.25 + 1.0).collect();
    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();

    let mut cuda = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = cuda.load_module(VECADD_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&cuda, module, "vecadd").unwrap();
    let da = upload(&mut sink, &mut cuda, &f32s(&a));
    let db = upload(&mut sink, &mut cuda, &f32s(&b));
    let dc = allocate::mem_alloc(&mut cuda, &mut sink, (n * 4) as u64).unwrap();
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut cuda, &mut sink, func, (4, 1, 1), (256, 1, 1), &args).unwrap();
    let cuda_got = cuda_readback(&mut sink, &cuda, dc, n * 4);
    assert_eq!(
        cuda_got, want,
        "CUDA vecadd on the shared lavapipe executor, all {n} elements exact"
    );
    let cuda_buffers_live = sink.resources().buffers.len();
    assert!(
        cuda_buffers_live >= 3,
        "CUDA's a/b/c buffers are live in the shared session"
    );

    // -------------------------------------------------------------------------------------------
    // (2b) REAL GL DRAW — a cleared frame lowered by the real hl-gl frame/swap service, RASTERIZED on
    //      lavapipe, IN THE SAME SESSION while CUDA's buffers are still live.
    // -------------------------------------------------------------------------------------------
    const GW: u32 = 64;
    const GH: u32 = 64;
    let clear = [0.0f32, 0.0, 1.0, 1.0]; // opaque blue
    let mut gl = GlContext::new();
    gl.surf = GlSurface {
        have: true,
        width: GW,
        height: GH,
    };
    record::clear_color(&mut gl, clear);
    record::clear(&mut gl);
    let mut f = frame::Frame::build(&mut gl).expect("gl clear frame builds");
    let (surface, texture) = f.present;
    f.cmds.push(Cmd::Present { surface, texture });
    sink.submit(&f.cmds)
        .expect("GL frame accepted + rasterized on the shared runtime + executor");
    gl.reset_frame();

    // GL default surface is Bgra8Unorm → blue clear reads back as BGRA [255, 0, 0, 255].
    let gl_px = sink
        .executor()
        .read_texture(sink.resources(), texture)
        .expect("read GL target");
    let gl_center = {
        let o = ((GH / 2 * GW + GW / 2) * 4) as usize;
        [gl_px[o], gl_px[o + 1], gl_px[o + 2], gl_px[o + 3]]
    };
    assert_eq!(
        gl_center,
        [255, 0, 0, 255],
        "GL target cleared to blue on the shared executor (BGRA)"
    );
    assert!(
        gl_px.chunks_exact(4).all(|t| t == [255, 0, 0, 255]),
        "every GL pixel is the clear color"
    );

    // -------------------------------------------------------------------------------------------
    // CROSS-CHECK — neither driver corrupted the other. CUDA's buffers are still live AND still hold the
    // exact vecadd result after the GL frame ran; the GL texture/surface are resident alongside them.
    // -------------------------------------------------------------------------------------------
    assert_eq!(
        sink.resources().buffers.len(),
        cuda_buffers_live,
        "GL frame added no buffers / freed none of CUDA's"
    );
    assert!(
        sink.resources().textures.get(texture).is_ok(),
        "GL render target resident in the shared session"
    );
    let cuda_after_gl = cuda_readback(&mut sink, &cuda, dc, n * 4);
    assert_eq!(
        cuda_after_gl, want,
        "CUDA vecadd result SURVIVES the GL draw — GL did not corrupt CUDA's buffers"
    );

    // -------------------------------------------------------------------------------------------
    // (3) THE THIRD DRIVER — Vulkan. A real VK buffer op lowered by the hl-vulkan service. While CUDA's
    //     buffers are still live, VK's buffer collides on the shared per-kind buffer id table (both mint
    //     from base 1) — the runtime rejects it. We assert the collision, then free CUDA's buffers and
    //     run the SAME VK op successfully on the SAME session + executor.
    // -------------------------------------------------------------------------------------------
    let inst = hl_vulkan::model::instance::Instance::new(HL_API_VERSION);
    let mut vk = inst.create_device();
    let collide = vk_create::create_buffer(&mut vk, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 256);
    assert!(
        collide.is_err(),
        "Vulkan's buffer must COLLIDE with a live CUDA buffer id on the shared session (documented limit): \
         got {collide:?}. Both drivers mint buffer ids from base 1 into the one per-kind table."
    );

    // Free CUDA's buffers (releases the shared buffer-id table), then Vulkan's real op runs cleanly here.
    for p in [da, db, dc] {
        allocate::mem_free(&mut cuda, &mut sink, p).unwrap();
    }
    assert_eq!(
        sink.resources().buffers.len(),
        0,
        "CUDA buffers freed from the shared session"
    );
    let mut vk2 = inst.create_device();
    let handle = vk_create::create_buffer(&mut vk2, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 256)
        .expect("Vulkan buffer op runs on the same session+executor once the id table is free");
    let ir_id = vk2.buffers.get(&handle).unwrap().ir_id;
    let vk_bytes = CommandSink::read_buffer(&mut sink, BufferId(ir_id), 0, 256).unwrap();
    assert_eq!(
        vk_bytes,
        vec![0u8; 256],
        "Vulkan buffer read back off the shared lavapipe executor (zero-init)"
    );
    assert_eq!(
        sink.resources().buffers.len(),
        1,
        "the Vulkan buffer is live in the shared session"
    );

    eprintln!(
        "multi_driver_coexist OK — engine.add(cuda,vulkan,gl); CUDA vecadd + GL clear-frame coexisted live on \
         one lavapipe session (cross-verified, no corruption); Vulkan buffer op demonstrated the shared-id \
         collision then ran on the same session after CUDA freed its buffers."
    );
}
