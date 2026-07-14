//! On-device verification for the wgpu backend: build hl-GPU IR streams, replay them through
//! [`hl_gpu_wgpu::WgpuBackend`] into a wgpu-owned render target, read the pixels back, and assert.
//!
//! Two cases mirror the shapes the bespoke Metal replay's golden suite uses:
//!   1. a flat vertex-color quad over a gray clear (builtin FLAT pipeline + rt_adjust uniform), and
//!   2. a textured full-frame quad sampling an 8x8 atlas (builtin TEX pipeline + sampler + CopyBufferToTexture).
//! This exercises CreateBuffer/WriteBuffer/CreateTexture/CreateSampler/CreateShader(->builtin)/
//! CreateRenderPipeline/CreateBindGroup/Submit(BeginRenderPass/SetPipeline/SetBindGroup/SetViewport/
//! SetScissor/SetVertexBuffer/[SetIndexBuffer]/Draw[Indexed]/EndRenderPass/CopyBufferToTexture) + readback.
//!
//! Run on the macOS host: `cargo run --release -p hl-gpu-wgpu --example verify_ir` (exit 0 = PASS).

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("verify_ir: macOS/Metal only");
}

#[cfg(target_os = "macos")]
fn main() {
    use hl_gpu::ir::*;
    use hl_gpu_wgpu::WgpuBackend;

    let mut failures = 0u32;

    fn f32s(out: &mut Vec<u8>, vals: &[f32]) {
        for v in vals {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    fn rt_adjust(w: u32, h: u32) -> [f32; 4] {
        [2.0 / w as f32, -1.0, 2.0 / h as f32, -1.0]
    }
    // MSL-as-bytes payload (the legacy shim shape) — forces the builtin-WGSL fallback path in the backend.
    fn pack_msl(src: &str) -> Vec<u32> {
        let mut words = vec![src.len() as u32];
        for ch in src.as_bytes().chunks(4) {
            let mut w = [0u8; 4];
            w[..ch.len()].copy_from_slice(ch);
            words.push(u32::from_le_bytes(w));
        }
        words
    }
    const FLAT_MSL: &str = "#include <metal_stdlib>\n[[stage_in]] flat";
    const TEX_MSL: &str = "#include <metal_stdlib>\n[[stage_in]] tex";

    const W: u32 = 64;
    const H: u32 = 64;

    let make_backend = || -> WgpuBackend {
        let mut be = WgpuBackend::new().expect("wgpu backend");
        let tex = be.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        be.set_render_target(1, tex, TextureFormat::Rgba8Unorm);
        be
    };

    let px = |data: &[u8], x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };

    // ---- case 1: flat centered red quad over gray clear -------------------------------------------
    {
        let mut verts = Vec::new();
        // centered quad pixels (16,16)-(48,48), two triangles, solid red.
        let corners = [(16.0, 16.0), (16.0, 48.0), (48.0, 16.0), (48.0, 16.0), (16.0, 48.0), (48.0, 48.0)];
        for (x, y) in corners {
            f32s(&mut verts, &[x, y]);
            f32s(&mut verts, &[1.0, 0.0, 0.0, 1.0]);
        }
        let mut unif = Vec::new();
        f32s(&mut unif, &rt_adjust(W, H));

        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: verts.len() as u64, usage: buffer_usage::VERTEX, label: "v".into() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: verts },
            Cmd::CreateBuffer(12, BufferDesc { size: unif.len() as u64, usage: buffer_usage::UNIFORM, label: "u".into() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: unif.clone() },
            Cmd::CreateShader { id: 20, kind: hl_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl(FLAT_MSL) },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 24,
                    step_mode: 0,
                    attrs: vec![
                        VertexAttr { location: 0, format: 2, offset: 0 },
                        VertexAttr { location: 1, format: 4, offset: 8 },
                    ],
                }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xf }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: "flat".into(),
            }),
            Cmd::CreateBindGroup(40, BindGroupDesc {
                set: 0,
                entries: vec![BindEntry { binding: 1, resource: BindResource::Buffer { id: 12, offset: 0, size: unif.len() as u64 } }],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.1, 0.1, 0.1, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: W as f32, h: H as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w: W, h: H },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: 6, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let ir = encode_stream(&cmds);
        let mut be = make_backend();
        hl_gpu::replay::replay_stream(&mut be, &ir).expect("replay flat");
        let data = be.read_target(1).expect("readback");
        let center = px(&data, 32, 32);
        let corner = px(&data, 2, 2);
        let is_red = center[0] > 240 && center[1] < 16 && center[2] < 16;
        let is_gray = (20..=40).contains(&corner[0]) && (20..=40).contains(&corner[1]);
        println!("[flat] center(32,32)={center:?} corner(2,2)={corner:?}");
        if is_red { println!("[flat] PASS center is red"); } else { println!("[flat] FAIL center != red"); failures += 1; }
        if is_gray { println!("[flat] PASS corner is gray clear"); } else { println!("[flat] FAIL corner != gray"); failures += 1; }
    }

    // ---- case 2: textured full-frame quad sampling an 8x8 atlas -----------------------------------
    {
        // 8x8 atlas: white ink where a checker cell is set, dark otherwise.
        let mut atlas = Vec::with_capacity(8 * 8 * 4);
        for r in 0..8u32 {
            for c in 0..8u32 {
                if (r + c) % 2 == 0 {
                    atlas.extend_from_slice(&[255, 255, 255, 255]);
                } else {
                    atlas.extend_from_slice(&[20, 20, 40, 255]);
                }
            }
        }
        // Full-frame quad: 4 verts stride 16 (float2 pos, uchar4 color, ushort2 texcoord), indices 0,1,2,2,1,3.
        let verts: [([f32; 2], [u16; 2]); 4] = [
            ([0.0, 0.0], [0, 0]),
            ([0.0, H as f32], [0, 8]),
            ([W as f32, 0.0], [8, 0]),
            ([W as f32, H as f32], [8, 8]),
        ];
        let mut vb = Vec::new();
        for (p, t) in verts {
            f32s(&mut vb, &p);
            vb.extend_from_slice(&[255, 255, 255, 255]);
            vb.extend_from_slice(&t[0].to_le_bytes());
            vb.extend_from_slice(&t[1].to_le_bytes());
        }
        let mut ib = Vec::new();
        for idx in [0u16, 1, 2, 2, 1, 3] {
            ib.extend_from_slice(&idx.to_le_bytes());
        }
        let mut unif = Vec::new();
        f32s(&mut unif, &rt_adjust(W, H));
        f32s(&mut unif, &[1.0 / 8.0, 1.0 / 8.0, 0.0, 0.0]);

        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: vb.len() as u64, usage: buffer_usage::VERTEX, label: "v".into() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: vb },
            Cmd::CreateBuffer(11, BufferDesc { size: ib.len() as u64, usage: buffer_usage::INDEX, label: "i".into() }),
            Cmd::WriteBuffer { id: 11, offset: 0, data: ib },
            Cmd::CreateBuffer(12, BufferDesc { size: unif.len() as u64, usage: buffer_usage::UNIFORM, label: "u".into() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: unif.clone() },
            Cmd::CreateBuffer(13, BufferDesc { size: atlas.len() as u64, usage: buffer_usage::COPY_SRC, label: "atlas-stg".into() }),
            Cmd::WriteBuffer { id: 13, offset: 0, data: atlas },
            Cmd::CreateTexture(2, TextureDesc {
                width: 8, height: 8, depth: 1, mip_levels: 1, sample_count: 1,
                dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::SAMPLED | texture_usage::COPY_DST, label: "atlas".into(),
            }),
            Cmd::CreateSampler(3, SamplerDesc {
                min_filter: Filter::Nearest, mag_filter: Filter::Nearest, mip_filter: Filter::Nearest,
                address_u: AddressMode::ClampToEdge, address_v: AddressMode::ClampToEdge, address_w: AddressMode::ClampToEdge,
            }),
            Cmd::CreateShader { id: 20, kind: hl_gpu::ir::ShaderPayloadKind::LegacyMsl, spirv: pack_msl(TEX_MSL) },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 16,
                    step_mode: 0,
                    attrs: vec![
                        VertexAttr { location: 0, format: 2, offset: 0 },            // float2
                        VertexAttr { location: 1, format: 0x1_0104, offset: 8 },     // uchar4-normalized
                        VertexAttr { location: 2, format: 0x0302, offset: 12 },      // ushort2
                    ],
                }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xf }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: "tex".into(),
            }),
            Cmd::CreateBindGroup(40, BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry { binding: 1, resource: BindResource::Buffer { id: 12, offset: 0, size: unif.len() as u64 } },
                    BindEntry { binding: 0, resource: BindResource::Texture { id: 2 } },
                    BindEntry { binding: 0, resource: BindResource::Sampler { id: 3 } },
                ],
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture { src: 13, src_offset: 0, bytes_per_row: 8 * 4, dst: 2, mip: 0, width: 8, height: 8 },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetViewport { x: 0.0, y: 0.0, w: W as f32, h: H as f32, min_depth: 0.0, max_depth: 1.0 },
                    Enc::SetScissor { x: 0, y: 0, w: W, h: H },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::SetIndexBuffer { buffer: 11, offset: 0, format: IndexFormat::U16 },
                    Enc::DrawIndexed { index_count: 6, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let ir = encode_stream(&cmds);
        let mut be = make_backend();
        hl_gpu::replay::replay_stream(&mut be, &ir).expect("replay tex");
        let data = be.read_target(1).expect("readback");
        // Orientation-agnostic: prove the atlas actually sampled by finding both ink and dark cells.
        let mut white = 0u32;
        let mut dark = 0u32;
        for i in (0..data.len()).step_by(4) {
            let p = [data[i], data[i + 1], data[i + 2]];
            if p[0] > 240 && p[1] > 240 && p[2] > 240 { white += 1; }
            if p[0] < 40 && p[1] < 40 && (28..=52).contains(&p[2]) { dark += 1; }
        }
        println!("[tex] white_px={white} darkish_px={dark}");
        if white > 100 && dark > 100 {
            println!("[tex] PASS sampled both ink and dark cells");
        } else {
            println!("[tex] FAIL atlas not sampled as expected");
            failures += 1;
        }
    }

    println!("\n==== verify_ir: {} ====", if failures == 0 { "PASS" } else { "FAIL" });
    std::process::exit(if failures == 0 { 0 } else { 1 });
}
