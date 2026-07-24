//! Regression: reading back a MULTISAMPLED texture must return a typed error, never panic the executor.
//!
//! A multisampled texture is built RENDER_ATTACHMENT-only (WebGPU forbids COPY usage on sampleCount>1), so
//! `copy_texture_to_buffer` against it is a hard wgpu validation error. Before the guard,
//! `read_texture_tight_mip` issued that copy unconditionally; wgpu's global error handler turned it into a
//! PANIC that unwound the executor's submit handler, the serve loop caught it and NACKed the whole frame,
//! and the guest saw a spurious DEVICE_LOST. This was the actual Zed blocker: the GPUI/wgpu path creates a
//! 4× MSAA color target, the test harness `capture()` reads back every live texture, and that readback
//! aborted the executor. A readback request must NEVER do that — it must be a clean typed error the caller
//! can skip.
//!
//! Skips cleanly if no adapter (lavapipe/Vulkan ICD) is reachable, mirroring the rest of the suite.

mod gpu_harness;

use gpu_harness::new_session;

use hl_gpu::protocol::model::descriptor::TextureDesc;
use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
use hl_gpu::{Cmd, GpuError};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn tex(w: u32, h: u32, sample_count: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

#[test]
fn reading_back_a_multisampled_texture_is_unsupported_not_a_panic() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return, // no adapter — skip
    };
    let mut s = new_session(&exec);

    // A 4× MSAA color target (RENDER_TARGET only — exactly what Zed's wgpu path creates).
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[Cmd::CreateTexture(
            1,
            tex(64, 64, 4, texture_usage::RENDER_TARGET),
        )],
    )
    .expect("creating a multisampled render target must succeed");

    // The readback that used to panic the executor thread. It must be a clean typed error instead.
    match exec.read_texture(&s.resources, 1) {
        Err(GpuError::Unsupported(_)) => {}
        Err(other) => panic!("expected Unsupported for MSAA readback, got {other:?}"),
        Ok(_) => {
            panic!("MSAA readback unexpectedly succeeded — a multisampled texture cannot be copied")
        }
    }

    // Crucial: the executor SURVIVED that guarded readback (it did not panic/poison). A subsequent normal
    // single-sample texture must still create and read back fine on the SAME executor.
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[Cmd::CreateTexture(
            2,
            tex(
                4,
                4,
                1,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        )],
    )
    .expect("the executor must still accept work after a guarded MSAA readback");
    let px = exec.read_texture(&s.resources, 2).expect(
        "a normal single-sample texture must still read back after the guarded MSAA readback",
    );
    assert_eq!(
        px.len(),
        4 * 4 * 4,
        "single-sample readback returns a tight RGBA plane"
    );
}
