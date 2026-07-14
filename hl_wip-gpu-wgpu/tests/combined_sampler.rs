//! Combined image-sampler end-to-end: a glslang-style SPIR-V fragment shader that samples a combined
//! `sampler2D` (an `OpTypeSampledImage` global + `OpImageSampleImplicitLod`, NO `OpSampledImage` — exactly
//! what glslang emits for `texture(tex, uv)`) drives a real draw on the wgpu executor. Before the
//! `spirv_split` pre-pass naga's spv-in rejected this module (`InvalidId`); here the whole textured pipeline
//! runs and the sampled texel is read back off the render target — the gap that blocked vkcube and every
//! textured real Vulkan app (Zed included).

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("combined-sampler program must run cleanly");
    s
}

fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// Mint SPIR-V (all entry points) from a WGSL seed via naga (the guest SPIR-V ABI round trip the suite uses).
fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

/// Build a **combined** image-sampler fragment shader SPIR-V by hand, exactly as glslang lowers
/// `layout(binding=B) uniform sampler2D tex; layout(location=0) in vec2 uv; layout(location=0) out vec4 c;
/// void main(){ c = texture(tex, uv); }` — an `OpTypeSampledImage` `UniformConstant` variable, an `OpLoad`
/// of it, and an `OpImageSampleImplicitLod` that consumes the loaded combined value with NO `OpSampledImage`.
/// This is the shape naga's spv-in rejects and `spirv_split` rewrites.
pub fn combined_frag_spirv(binding: u32) -> Vec<u32> {
    // Fixed id numbering (bound = 19).
    const MAIN: u32 = 1;
    const UV: u32 = 2;
    const COLOR: u32 = 3;
    const TEX: u32 = 4;
    const VOID: u32 = 5;
    const FNTY: u32 = 6;
    const FLOAT: u32 = 7;
    const V2: u32 = 8;
    const V4: u32 = 9;
    const IMAGE: u32 = 10;
    const SAMPLED: u32 = 11;
    const PTR_UC_SAMPLED: u32 = 12;
    const PTR_IN_V2: u32 = 13;
    const PTR_OUT_V4: u32 = 14;
    const LABEL: u32 = 15;
    const LD_TEX: u32 = 16;
    const LD_UV: u32 = 17;
    const SAMPLE: u32 = 18;
    const BOUND: u32 = 19;

    let mut w: Vec<u32> = vec![0x0723_0203, 0x0001_0000, 0, BOUND, 0]; // magic, version 1.0, gen, bound, schema
    let mut push = |op: u16, ops: &[u32]| {
        w.push(((ops.len() as u32 + 1) << 16) | op as u32);
        w.extend_from_slice(ops);
    };
    push(17, &[1]); // OpCapability Shader
    push(14, &[0, 1]); // OpMemoryModel Logical GLSL450
    push(15, &[4, MAIN, 0x6E69_616D, 0x0000_0000, UV, COLOR]); // OpEntryPoint Fragment %main "main" %uv %color
    push(16, &[MAIN, 7]); // OpExecutionMode %main OriginUpperLeft
    push(71, &[UV, 30, 0]); // OpDecorate %uv Location 0
    push(71, &[COLOR, 30, 0]); // OpDecorate %color Location 0
    push(71, &[TEX, 34, 0]); // OpDecorate %tex DescriptorSet 0
    push(71, &[TEX, 33, binding]); // OpDecorate %tex Binding B
    push(19, &[VOID]); // OpTypeVoid
    push(33, &[FNTY, VOID]); // OpTypeFunction %void
    push(22, &[FLOAT, 32]); // OpTypeFloat 32
    push(23, &[V2, FLOAT, 2]); // OpTypeVector %float 2
    push(23, &[V4, FLOAT, 4]); // OpTypeVector %float 4
    push(25, &[IMAGE, FLOAT, 1, 0, 0, 0, 1, 0]); // OpTypeImage %float 2D depth=0 arr=0 ms=0 sampled=1 Unknown
    push(27, &[SAMPLED, IMAGE]); // OpTypeSampledImage %image
    push(32, &[PTR_UC_SAMPLED, 0, SAMPLED]); // OpTypePointer UniformConstant %sampled
    push(59, &[PTR_UC_SAMPLED, TEX, 0]); // OpVariable %ptr UniformConstant  (the COMBINED sampler)
    push(32, &[PTR_IN_V2, 1, V2]); // OpTypePointer Input %v2float
    push(59, &[PTR_IN_V2, UV, 1]); // OpVariable %ptr Input
    push(32, &[PTR_OUT_V4, 3, V4]); // OpTypePointer Output %v4float
    push(59, &[PTR_OUT_V4, COLOR, 3]); // OpVariable %ptr Output
    push(54, &[VOID, MAIN, 0, FNTY]); // OpFunction %void None %fnty
    push(248, &[LABEL]); // OpLabel
    push(61, &[SAMPLED, LD_TEX, TEX]); // OpLoad %sampled %tex  (loads the combined value)
    push(61, &[V2, LD_UV, UV]); // OpLoad %v2float %uv
    push(87, &[V4, SAMPLE, LD_TEX, LD_UV]); // OpImageSampleImplicitLod %v4 %ld_tex %ld_uv  (no OpSampledImage!)
    push(62, &[COLOR, SAMPLE]); // OpStore %color %sample
    push(253, &[]); // OpReturn
    push(56, &[]); // OpFunctionEnd
    w
}

/// A vertex shader emitting a fullscreen triangle with a constant uv (0.5,0.5), sampling the 1×1 source
/// texture's single texel. Its `@location(0)` uv matches the fragment's `in vec2 uv`.
const VS_SEED: &str = r#"
    struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
    @vertex fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
        var p = array<vec2<f32>, 3>(vec2<f32>(-1.0,-1.0), vec2<f32>(3.0,-1.0), vec2<f32>(-1.0,3.0));
        var o: VOut;
        o.pos = vec4<f32>(p[vi], 0.0, 1.0);
        o.uv = vec2<f32>(0.5, 0.5);
        return o;
    }
"#;

#[test]
fn combined_sampler_frag_samples_texture_through_the_split() {
    // The combined descriptor lives at Vulkan binding 0 → after the split the image binds at 0 and the
    // sampler at 0 + SAMPLER_BINDING_OFFSET(16), the coordination the Vulkan driver mirrors.
    const IMAGE_BINDING: u32 = 0;
    const SAMPLER_BINDING: u32 = 16;
    let src_color: [u8; 4] = [40, 160, 210, 255];

    let vs = wgsl_to_spirv(VS_SEED);
    let fs = combined_frag_spirv(IMAGE_BINDING);

    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            // Render target (id 1) + a 1×1 sampled source texture (id 2) seeded with a known texel (id 3 buf).
            Cmd::CreateTexture(1, tex(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            Cmd::CreateTexture(2, tex(1, 1, texture_usage::COPY_DST)),
            Cmd::CreateBuffer(1, BufferDesc { size: 4, usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: src_color.to_vec() },
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: vs },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::SpirV, spirv: fs }, // <- combined sampler path
            Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: Filter::Nearest,
                    mag_filter: Filter::Nearest,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                },
            ),
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vs_main".into() },
                    fragment: Some(ShaderRef { module: 2, entry: "main".into() }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            // The split layout: texture at binding 0, sampler at binding 16 (what the driver emits too).
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry { binding: IMAGE_BINDING, resource: BindResource::Texture { id: 2 } },
                        BindEntry { binding: SAMPLER_BINDING, resource: BindResource::Sampler { id: 1 } },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // Seed the source texel, then draw the sampling triangle over the whole target.
                    Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 4, dst: 2, mip: 0, width: 1, height: 1 },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );

    let px = g.read_texture(&s.resources, 1).unwrap();
    for (i, texel) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            texel, src_color,
            "pixel {i}: the combined-sampler fragment shader must sample the source texel {src_color:?}, \
             proving the combined→separate SPIR-V split executed end to end"
        );
    }
}
