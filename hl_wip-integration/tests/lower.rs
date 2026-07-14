//! CAPSTONE goal #2 — all three drivers lower onto the ONE runtime + reference CPU executor.
//!
//! One `InProcessCommandSink<CpuExecutor>` is built and SHARED across all three real drivers. Each
//! driver drives a tiny op through its REAL lowering services and the shared sink runs the whole neutral
//! host pipeline (validate -> account -> dispatch -> `CpuExecutor::execute`) over one runtime `Session`:
//!
//!   * CUDA   `cuMemAlloc`         -> `Cmd::CreateBuffer` -> a live runtime buffer, read back off the sink
//!   * Vulkan `vkCreateBuffer`     -> `Cmd::CreateBuffer` -> a live runtime buffer, read back off the sink
//!   * GL     `glClear` + swap     -> a frame (`CreateTexture`/`CreateSurface`/render-pass/`Present`) whose
//!                                    cleared target is read back and asserted to be the clear color
//!
//! The exact emitted `Cmd` is asserted separately against a `RecordingSink` (the tested lowering surface),
//! then the SAME op is run through the shared in-process sink to prove the neutral runtime + CPU executor
//! actually ACCEPTED and EXECUTED it. Because all three share one sink (one `Session`, one `CpuExecutor`),
//! this is the end-to-end proof that CUDA, Vulkan, and GL all lower onto the single neutral runtime.

use hl_cuda::service::allocate;
use hl_cuda::{CudaContext, CudaDeviceDesc};

use hl_vulkan::model::memory::vk_buffer_usage;
use hl_vulkan::result::HL_API_VERSION;
use hl_vulkan::service::create as vk_create;

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::service::{frame, record, swap};

use hl_gpu::{
    BufferId, Cmd, CommandSink, CpuExecutor, InProcessCommandSink, RecordingSink, TextureId,
};

const SIZE: u64 = 256;

/// Assert a batch's single command is a `CreateBuffer` of the expected id + size.
fn assert_create_buffer(batch: &[Cmd], want_id: u32, want_size: u64) {
    assert_eq!(batch.len(), 1, "one CreateBuffer command");
    match &batch[0] {
        Cmd::CreateBuffer(id, desc) => {
            assert_eq!(*id, want_id, "CreateBuffer id");
            assert_eq!(desc.size, want_size, "CreateBuffer size");
            assert!(desc.usage != 0, "CreateBuffer carries usage flags");
        }
        other => panic!("expected Cmd::CreateBuffer, got {other:?}"),
    }
}

#[test]
fn all_three_drivers_lower_onto_one_shared_runtime_and_cpu_executor() {
    // The ONE runtime + reference CPU executor every driver lowers onto. Built once, shared below.
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());

    // =============================================================================================
    // CUDA: cuMemAlloc -> Cmd::CreateBuffer, executed on the shared runtime + CPU executor.
    // =============================================================================================
    {
        // First assert the exact lowered Cmd against a RecordingSink (fresh context, buffer id 1).
        let mut rec_ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
        let mut rec = RecordingSink::with_full_caps();
        allocate::mem_alloc(&mut rec_ctx, &mut rec, SIZE).unwrap();
        assert_create_buffer(&rec.batches[0], 1, SIZE);

        // Now run the REAL op through the SHARED in-process sink and prove the runtime accepted it.
        let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
        let ptr = allocate::mem_alloc(&mut ctx, &mut sink, SIZE).unwrap();
        assert_eq!(sink.resources().buffers.len(), 1, "cuda: buffer is live in the shared session");

        // Readback off the shared executor proves the CreateBuffer really executed (zero-initialized).
        let (buf, off) = ctx.resolve(ptr).expect("device pointer resolves to a live buffer");
        let bytes = sink.read_buffer(buf, off, SIZE as usize).unwrap();
        assert_eq!(bytes, vec![0u8; SIZE as usize], "cuda: buffer read back off the shared CPU executor");

        // Free it so its buffer id (1) is released before the Vulkan buffer reuses id 1 in the same session.
        allocate::mem_free(&mut ctx, &mut sink, ptr).unwrap();
        assert_eq!(sink.resources().buffers.len(), 0, "cuda: buffer freed from the shared session");
    }

    // =============================================================================================
    // VULKAN: vkCreateBuffer -> Cmd::CreateBuffer, executed on the SAME shared runtime + executor.
    // =============================================================================================
    {
        // Exact lowered Cmd against a RecordingSink (fresh device, ir buffer id 1).
        let inst = vk_create::create_instance(HL_API_VERSION);
        let mut rec_dev = vk_create::create_device(&inst);
        let mut rec = RecordingSink::with_full_caps();
        vk_create::create_buffer(&mut rec_dev, &mut rec, vk_buffer_usage::VERTEX_BUFFER, SIZE).unwrap();
        assert_create_buffer(&rec.batches[0], 1, SIZE);

        // Real op through the SHARED sink.
        let mut dev = vk_create::create_device(&inst);
        let handle =
            vk_create::create_buffer(&mut dev, &mut sink, vk_buffer_usage::VERTEX_BUFFER, SIZE).unwrap();
        assert_eq!(sink.resources().buffers.len(), 1, "vulkan: buffer is live in the shared session");

        let ir_id = dev.buffers.get(&handle).unwrap().ir_id;
        let bytes = sink.read_buffer(BufferId(ir_id), 0, SIZE as usize).unwrap();
        assert_eq!(bytes, vec![0u8; SIZE as usize], "vulkan: buffer read back off the shared CPU executor");

        vk_create::destroy_buffer(&mut dev, &mut sink, handle).unwrap();
        assert_eq!(sink.resources().buffers.len(), 0, "vulkan: buffer freed from the shared session");
    }

    // =============================================================================================
    // GL: glClear + eglSwapBuffers -> a whole frame, executed on the SAME shared runtime + executor.
    // =============================================================================================
    {
        const W: u32 = 8;
        const H: u32 = 8;
        let clear = [0.0f32, 0.0, 1.0, 1.0]; // opaque blue

        // Exact lowered frame against a RecordingSink: the real swap service must emit a render-pass
        // Submit and a Present of the cleared target.
        let mut rec_ctx = GlContext::new();
        rec_ctx.surf = GlSurface { have: true, width: W, height: H };
        let mut rec = RecordingSink::with_full_caps();
        record::clear_color(&mut rec_ctx, clear);
        record::clear(&mut rec_ctx);
        assert!(swap::swap_buffers(&mut rec_ctx, &mut rec).unwrap(), "gl: a frame was presented");
        let frame_cmds = &rec.batches[0];
        assert!(
            frame_cmds.iter().any(|c| matches!(c, Cmd::Submit(_))),
            "gl: frame carries a render-pass Submit",
        );
        assert!(
            frame_cmds.iter().any(|c| matches!(c, Cmd::Present { .. })),
            "gl: frame ends by presenting the cleared target",
        );

        // Real frame through the SHARED sink. Build it manually so we capture the presented texture id to
        // read back (this is exactly the eglSwapBuffers body: build_frame_ir + Present + submit).
        let mut ctx = GlContext::new();
        ctx.surf = GlSurface { have: true, width: W, height: H };
        record::clear_color(&mut ctx, clear);
        record::clear(&mut ctx);

        let mut f = frame::build_frame_ir(&mut ctx).expect("a clear frame to present");
        let (surface, texture) = f.present;
        f.cmds.push(Cmd::Present { surface, texture });
        sink.submit(&f.cmds).expect("gl: frame accepted + executed by the shared runtime");
        ctx.reset_frame();

        // The clear pass ran on the SAME CPU executor: read the presented Bgra8 target back — blue clear
        // stored as BGRA is [255, 0, 0, 255].
        let mut px = vec![0u8; (W * H * 4) as usize];
        sink.executor()
            .read_texture(sink.resources(), TextureId(texture), &mut px)
            .expect("read back the cleared GL target");
        let center = {
            let o = ((H / 2 * W + W / 2) * 4) as usize;
            [px[o], px[o + 1], px[o + 2], px[o + 3]]
        };
        assert_eq!(center, [255, 0, 0, 255], "gl: target cleared to blue on the shared CPU executor (BGRA)");
        assert!(
            px.chunks_exact(4).all(|t| t == [255, 0, 0, 255]),
            "gl: every pixel is the clear color",
        );
    }

    // The shared session ends clean: the CUDA + Vulkan buffers were freed; the GL frame left its
    // presentable surface + render target resident — all on the ONE runtime the three drivers share.
    assert_eq!(sink.resources().buffers.len(), 0, "no buffers leaked across the three drivers");
    assert!(sink.resources().surfaces.len() >= 1, "the GL frame's surface is resident on the shared runtime");
    assert!(sink.resources().textures.len() >= 1, "the GL frame's render target is resident on the shared runtime");
}
