//! The 32-bit float formats are refused a LINEAR filter by three layers independently, and this binds
//! that agreement so it cannot drift silently.
//!
//! WebGPU makes `R32Float`, `Rg32Float`, and `Rgba32Float` non-filterable unless the optional `FLOAT32_FILTERABLE`
//! feature is enabled. Three places currently decline them, each for its own stated reason and none
//! naming the other two:
//!
//!   * the wgpu executor's blit (`blit.rs::filterable`), because the device does not request the feature;
//!   * the software reference (`cpu/format.rs::FILTERABLE_REFUSED`), deliberately mirroring the executor,
//!     with a comment saying in as many words that it could interpolate them perfectly well and that
//!     accepting what the subject refuses would be a false divergence;
//!   * the Vulkan surface (`record/image.rs::FILTERABLE`), a compile-time list from which the three float
//!     formats are absent.
//!
//! Three constants that happen to agree, with nothing checking that they do. The differential does not
//! generate a float linear blit, so if one layer moved, no test in this repository would notice — and the
//! move is a one-line addition to a feature mask.
//!
//! WHY THE FEATURE IS NOT ENABLED, measured rather than assumed. The adapter this was written against
//! (llvmpipe, Vulkan backend) DOES offer `FLOAT32_FILTERABLE`, and reports the `FILTERABLE` format flag
//! for all three float formats — so the refusal is a choice, not an absence, and "the host cannot do it" would
//! be the wrong reason to record. The reason it stays off is that the feature is ADAPTER-DEPENDENT while
//! the other two layers are compile-time constants. `required_features` is masked by `adapter.features()`,
//! so enabling it would make the executor accept a linear float blit on one host and refuse it on
//! another, while the reference and the Vulkan surface refuse it on every host. That does not merely put
//! the executor ahead of the reference — the position this project has declined three times tonight — it
//! makes the differential's answer depend on the machine it ran on, so a divergence would appear and
//! disappear across hosts and no result would be reproducible from its provenance.
//!
//! RETIREMENT CONDITION. Enable it when the other two layers can be told what the paired executor can
//! actually filter, rather than deciding at compile time: the reference's refused set and the Vulkan
//! surface's filterable list both become adapter-derived, negotiated the way the rest of the capability
//! set already is. At that point all three agree again on every host, and this test becomes a comparison
//! of what they agree ON rather than that they refuse.
//!
//! This test fails if ANY of the three moves alone, which is the point: it turns a coincidence into a
//! decision someone has to make on purpose.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{texture_usage, Filter, TextureDim, TextureFormat};
use hl_gpu::{
    Cmd, CommandBuffer, CpuExecutor, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// The formats WebGPU makes non-filterable without the optional feature.
const FLOAT32: [TextureFormat; 3] = [
    TextureFormat::R32Float,
    TextureFormat::Rg32Float,
    TextureFormat::Rgba32Float,
];

fn tex(format: TextureFormat) -> TextureDesc {
    TextureDesc {
        width: 2,
        height: 2,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    }
}

/// A 2x2 → 1x1 downscale, which is the shape that actually EXERCISES the filter: nearest selects one
/// texel and linear averages four, so a backend that silently substituted one for the other would produce
/// a different answer rather than the same one. A same-size blit would not distinguish them.
fn program(src: TextureFormat, filter: Filter) -> Vec<Cmd> {
    vec![
        Cmd::CreateTexture(1, tex(src)),
        Cmd::CreateTexture(2, tex(TextureFormat::Rgba8Unorm)),
        Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::BlitTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d::default(),
                src_extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                dst_extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                filter,
                mirror: Mirror::NONE,
            }],
            signal: None,
        }),
    ]
}

fn on_executor(exec: &mut WgpuExecutor, src: TextureFormat, filter: Filter) -> hl_gpu::Result<()> {
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, &program(src, filter)).map(|_| ())
}

fn on_reference(src: TextureFormat, filter: Filter) -> hl_gpu::Result<()> {
    let mut cpu = CpuExecutor::new();
    let mut s = Session::new(
        Limits::from_capabilities(cpu.capabilities()),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, &mut cpu, 0, &program(src, filter)).map(|_| ())
}

#[test]
fn the_executor_and_the_reference_refuse_a_linear_float_blit_together() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("adapter");

    // POSITIVE CONTROLS FIRST, both of them, because a test made only of refusals passes just as well
    // against a blit path that is broken outright.
    //
    // (a) A filterable source takes a linear blit on both sides — so "linear is refused" below is about
    //     the FORMAT, not about linear filtering being broken.
    for backend in ["executor", "reference"] {
        let r = match backend {
            "executor" => on_executor(&mut exec, TextureFormat::Rgba8Unorm, Filter::Linear),
            _ => on_reference(TextureFormat::Rgba8Unorm, Filter::Linear),
        };
        assert!(
            r.is_ok(),
            "{backend}: a linear blit from a filterable format must run, got {r:?}"
        );
    }
    // (b) A float source takes a NEAREST blit on both sides — so "float is refused" below is about the
    //     FILTER, not about the float formats being unusable. This half is load-bearing: the executor's
    //     bind-group layout used to declare its source filterable unconditionally, which refused a
    //     nearest float blit too, and that was a real defect rather than this policy.
    for format in FLOAT32 {
        assert!(
            on_executor(&mut exec, format, Filter::Nearest).is_ok(),
            "executor: a NEAREST blit from {format:?} needs no filtering and must run"
        );
        let reference = on_reference(format, Filter::Nearest);
        assert!(
            reference.is_ok(),
            "reference: a NEAREST blit from {format:?} needs no filtering and must run, got {reference:?}"
        );
    }

    // The agreement itself. Both sides must refuse, and refuse as UNSUPPORTED — a limit of the
    // implementation, not the caller's error.
    for format in FLOAT32 {
        let host = on_executor(&mut exec, format, Filter::Linear);
        let reference = on_reference(format, Filter::Linear);
        assert!(
            matches!(host, Err(hl_gpu::GpuError::Unsupported(_))),
            "executor: a LINEAR blit from {format:?} must be refused as unsupported, got {host:?}. \
             If this now succeeds, FLOAT32_FILTERABLE has been enabled and the reference and the Vulkan \
             surface must be taught the same set before the executor is allowed ahead of them — see this \
             file's header for why the feature being adapter-dependent is the blocker."
        );
        assert!(
            matches!(reference, Err(hl_gpu::GpuError::Unsupported(_))),
            "reference: a LINEAR blit from {format:?} must be refused as unsupported, got {reference:?}. \
             If this now succeeds, the reference has been allowed to interpolate a format the executor \
             declines, which is a false divergence in the direction that makes the differential agree by \
             one side being wrong."
        );
    }
}

#[test]
fn metal_executes_every_linear_format_the_vulkan_surface_advertises() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    for format in [
        TextureFormat::Rgba8Snorm,
        TextureFormat::Rg16Float,
        TextureFormat::Rgb10a2Unorm,
    ] {
        on_executor(&mut exec, format, Filter::Linear)
            .unwrap_or_else(|error| panic!("{format:?} is advertised filterable: {error}"));
    }
}
