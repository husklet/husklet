//! Capability HONESTY audit: everything `WgpuExecutor` advertises must be genuinely backed, and nothing it
//! can't do may be advertised. This is the "advertised == implemented" cross-check the coverage task asks
//! for — a capability lie (a negotiated-but-unhandled command / format / payload) would let a guest commit
//! to a path that silently no-ops.
//!
//! Checked here:
//!   * wire_version == the protocol's `WIRE_VERSION`;
//!   * command_bits == the FULL encoder-op set (`ALL_COMMANDS`) — every command is advertised, and the rest
//!     of this suite proves each one actually executes (no `_ => noop` swallow);
//!   * texture_formats == the exact color+depth+stencil union, and EVERY advertised format is really
//!     creatable on the device (no advertised-but-unallocatable format);
//!   * shader_payloads == SPIRV | GLSL | KERNEL, and MSL / WGSL are NOT advertised (there is no wire path to
//!     accept them — see `lib.rs`);
//!   * supports_timeline_fences == false, advertised truthfully: fences are emulated via submission
//!     completion, which still services a wait for a signalled value.
//!
//! Skips with no adapter.

use hl_gpu::protocol::model::capability::{
    binding_array, shader_payload, PresentKind, ALL_COMMANDS, COLOR_FORMATS, DEPTH_FORMATS,
};
use hl_gpu::protocol::model::command::WIRE_VERSION;
use hl_gpu::protocol::model::descriptor::TextureDesc;
use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat};
use hl_gpu::Capabilities;
use hl_gpu::{Cmd, CommandBuffer, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

#[test]
fn advertised_wire_version_and_command_set_are_the_full_ir() {
    let exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let caps = exec.capabilities();
    assert_eq!(
        caps.wire_version, WIRE_VERSION,
        "advertised wire version must match the protocol"
    );
    // The executor advertises EVERY encoder command; the other suites prove each runs with a real handler.
    assert_eq!(
        caps.command_bits,
        Capabilities::command_bits(ALL_COMMANDS),
        "executor must advertise exactly the full encoder-command set (no missing/extra command bit)"
    );
    // Every advertised command bit must be a real etag (< 64, present in ALL_COMMANDS).
    for etag in 0u8..64 {
        if caps.command_bits & (1u64 << etag) != 0 {
            assert!(
                ALL_COMMANDS.contains(&etag),
                "advertised etag {etag} is not a real IR command"
            );
        }
    }
    assert_eq!(
        caps.max_bind_groups, 4,
        "advertised max_bind_groups pins the per-pass bind-group array size"
    );
    assert_ne!(
        caps.binding_arrays & binding_array::STORAGE_BUFFER,
        0,
        "storage-buffer arrays are scalarized into ordinary host bindings"
    );
    assert_ne!(
        caps.non_uniform_binding_arrays & binding_array::STORAGE_BUFFER,
        0,
        "dynamic storage-buffer indexing is bounded and scalarized"
    );
    assert_ne!(
        caps.binding_arrays & binding_array::STORAGE_TEXTURE,
        0,
        "storage-image arrays are scalarized into ordinary host bindings"
    );
    assert_ne!(
        caps.non_uniform_binding_arrays & binding_array::STORAGE_TEXTURE,
        0,
        "dynamic storage-image indexing is bounded and scalarized"
    );
    assert_eq!(
        caps.present_kinds,
        vec![PresentKind::Shm],
        "the wgpu backend presents only via shm"
    );
}

#[test]
fn advertised_shader_payloads_match_what_is_accepted() {
    let exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let caps = exec.capabilities();
    assert_eq!(
        caps.shader_payloads,
        shader_payload::SPIRV | shader_payload::GLSL | shader_payload::KERNEL,
        "the executor accepts SPIR-V, GLSL and neutral kernels — and (honestly) NOT MSL or WGSL"
    );
    assert!(caps.supports_shader_payload(shader_payload::SPIRV));
    assert!(caps.supports_shader_payload(shader_payload::GLSL));
    assert!(caps.supports_shader_payload(shader_payload::KERNEL));
    assert!(
        !caps.supports_shader_payload(shader_payload::MSL),
        "MSL must not be advertised (no wire path accepts it)"
    );
    assert!(
        !caps.supports_shader_payload(shader_payload::WGSL),
        "WGSL must not be advertised (no wire path accepts it)"
    );
}

#[test]
fn every_advertised_texture_format_is_really_creatable() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let caps = exec.capabilities();
    // The advertisement is exactly color ∪ integer ∪ depth ∪ combined-depth-stencil.
    let expect = TextureFormat::bits(COLOR_FORMATS)
        | TextureFormat::bits(hl_gpu::protocol::model::capability::INTEGER_FORMATS)
        | TextureFormat::bits(hl_gpu::protocol::model::capability::NATIVE_FORMATS)
        | TextureFormat::bits(DEPTH_FORMATS)
        | TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8])
        | (caps.texture_formats
            & TextureFormat::bits(hl_gpu::protocol::model::capability::BC_FORMATS))
        | (caps.texture_formats
            & TextureFormat::bits(hl_gpu::protocol::model::capability::ETC2_FORMATS));
    assert_eq!(
        caps.texture_formats, expect,
        "advertised texture formats must be exactly the backed union"
    );

    // Every format across the whole enum: if advertised it MUST allocate; the two depth formats are render
    // targets, the color formats are full-usage. This is the "no advertised-but-unallocatable format" check.
    let mut all = vec![
        TextureFormat::Rgba8Unorm,
        TextureFormat::Bgra8Unorm,
        TextureFormat::Rgba8Srgb,
        TextureFormat::Bgra8Srgb,
        TextureFormat::R8Unorm,
        TextureFormat::Rg8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
        TextureFormat::R32Float,
        TextureFormat::Depth32Float,
        TextureFormat::Depth24PlusStencil8,
    ];
    all.extend_from_slice(hl_gpu::protocol::model::capability::INTEGER_FORMATS);
    all.extend_from_slice(hl_gpu::protocol::model::capability::NATIVE_FORMATS);
    all.extend_from_slice(hl_gpu::protocol::model::capability::BC_FORMATS);
    all.extend_from_slice(hl_gpu::protocol::model::capability::ETC2_FORMATS);
    for fmt in all {
        if !caps.supports_format(fmt) {
            continue;
        }
        let is_depth = matches!(
            fmt,
            TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8
        );
        let compressed = fmt.block_geometry().is_some();
        let usage = if is_depth {
            texture_usage::RENDER_TARGET
        } else if compressed {
            texture_usage::SAMPLED | texture_usage::COPY_SRC | texture_usage::COPY_DST
        } else {
            texture_usage::SAMPLED
                | texture_usage::COPY_SRC
                | texture_usage::COPY_DST
                | texture_usage::RENDER_TARGET
        };
        let mut s = session(&exec);
        let desc = TextureDesc {
            width: if compressed { 4 } else { 2 },
            height: if compressed { 4 } else { 2 },
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: fmt,
            usage,
            label: String::new(),
        };
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::CreateTexture(1, desc)])
            .unwrap_or_else(|e| panic!("advertised format {fmt:?} must be creatable, got {e:?}"));
    }
}

#[test]
fn timeline_fences_advertised_false_but_emulated_wait_services_a_signal() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    assert!(
        !exec.capabilities().supports_timeline_fences,
        "the wgpu backend has no real external timeline fence"
    );

    // The emulated (completion-based) fence still services a wait for a signalled value...
    let mut s = session(&exec);
    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateFence(1),
            Cmd::Submit(CommandBuffer {
                encoder: vec![],
                signal: Some((1, 5)),
            }),
            Cmd::WaitFence { id: 1, value: 5 },
        ],
    )
    .expect("a wait for a signalled fence value must succeed");

    // ...and honestly refuses a wait for a value that was never signalled (no silent success).
    let mut s2 = session(&exec);
    let r = hl_gpu::runtime::submit(
        &mut s2,
        &mut exec,
        0,
        &[
            Cmd::CreateFence(1),
            Cmd::Submit(CommandBuffer {
                encoder: vec![],
                signal: Some((1, 5)),
            }),
            Cmd::WaitFence { id: 1, value: 6 },
        ],
    );
    assert!(
        r.is_err(),
        "a wait for a never-signalled fence value must error, not silently pass"
    );
}
