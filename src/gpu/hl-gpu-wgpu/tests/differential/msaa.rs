use super::runners::wgpu_session;
use super::*;

// =================================================================================================
// ANALYTIC MSAA-resolve checks (executor-vs-hand-computed — the oracle can NOT model multisampling)
// =================================================================================================
//
// The CPU oracle's `validate` rejects a `sample_count > 1` colour attachment, so it cannot PRODUCE a
// coverage-antialiased multisample render to compare against (it can average existing samples via
// `resolve_texture`, but there is no oracle path that fills those samples from a draw). Rather than fake an
// oracle result, the wgpu executor's 4× MSAA render + `ResolveTexture` is checked against a HAND-COMPUTED
// expectation:
//   * FULL coverage (a fullscreen triangle of one flat colour): every one of a pixel's 4 samples is that
//     colour, so the resolve average is exactly that colour — the whole readback equals the packed draw
//     colour (analytic, exact within ±1 unorm).
//   * HALF coverage (a right triangle whose hypotenuse is the main diagonal): the deep interior is fully
//     covered → exact fg, the deep exterior fully uncovered → exact bg (both analytic and sample-position
//     independent), and the diagonal edge MUST carry averaged "intermediate" pixels (a partial coverage the
//     hard 1× rasterizer never produces) — the structural proof the resolve averages sub-pixel coverage.

// A fullscreen-triangle vertex shader (vertex_index driven — no vertex buffer needed on the executor-only
// MSAA path) writing a constant colour whose channels are exact k/255 values, so the fragment→unorm8
// round-trip is lossless and the analytic expected byte is unambiguous.

const MSAA_FULL_WGSL: &str = r#"
    @vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
        var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
        return vec4<f32>(p[vi], 0.0, 1.0);
    }
    @fragment fn fs_main() -> @location(0) vec4<f32> {
        // 64/255, 128/255, 192/255 — exact unorm8 values (lossless round-trip).
        return vec4<f32>(0.250980392, 0.501960784, 0.752941176, 1.0);
    }
"#;
const MSAA_FULL_EXPECT: [u8; 4] = [64, 128, 192, 255];

// A right triangle whose hypotenuse is the framebuffer main diagonal (fy == fx): covered side is the
// lower-left half (fy > fx). White fg on the black clear, so a partially-covered edge pixel resolves to an
// unmistakable mid-gray.
const MSAA_HALF_WGSL: &str = r#"
    @vertex fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
        var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0));
        return vec4<f32>(p[vi], 0.0, 1.0);
    }
    @fragment fn fs_main() -> @location(0) vec4<f32> { return vec4<f32>(1.0, 1.0, 1.0, 1.0); }
"#;

/// Render a fullscreen (vertex_index) triangle from `wgsl` into a `sample_count`× `Rgba8Unorm` colour
/// target and, when multisampled, `ResolveTexture` it into a single-sample destination; return the
/// single-sample tight readback plane (`w*h*4`). Executor-only (there is no oracle multisample render).
fn msaa_render_resolve(
    exec: &mut WgpuExecutor,
    w: u32,
    h: u32,
    wgsl: &str,
    sample_count: u32,
) -> hl_gpu::Result<Vec<u8>> {
    fn ms_tex(w: u32, h: u32, sample_count: u32, usage: u32) -> TextureDesc {
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
    let spirv = wgsl_to_spirv(wgsl);
    let pipe = |sc: u32| {
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "vs_main".into(),
                },
                fragment: Some(ShaderRef {
                    module: 1,
                    entry: "fs_main".into(),
                }),
                vertex_buffers: vec![],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: sc,
                label: String::new(),
            },
        )
    };
    let mut s = wgpu_session(exec);
    let read_id = if sample_count > 1 {
        // A multisampled colour target is RENDER_TARGET-only (never copied, only resolved); id 2 is the
        // single-sample resolve destination that is read back.
        let cmds = vec![
            Cmd::CreateTexture(1, ms_tex(w, h, sample_count, texture_usage::RENDER_TARGET)),
            Cmd::CreateTexture(
                2,
                ms_tex(
                    w,
                    h,
                    1,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            pipe(sample_count),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                    Enc::ResolveTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: w,
                            height: h,
                            depth: 1,
                        },
                    },
                ],
                signal: None,
            }),
        ];
        hl_gpu::runtime::submit(&mut s, exec, 0, &cmds)?;
        2
    } else {
        let cmds = vec![
            Cmd::CreateTexture(
                1,
                ms_tex(
                    w,
                    h,
                    1,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            pipe(1),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        hl_gpu::runtime::submit(&mut s, exec, 0, &cmds)?;
        1
    };
    exec.read_texture(&s.resources, read_id)
}

/// Run the analytic MSAA-resolve checks. Returns `(checks_run, failures)`; `failures` is empty on success.
pub(super) fn analytic_msaa_resolve(exec: &mut WgpuExecutor) -> (u32, Vec<String>) {
    let mut checks = 0u32;
    let mut fails: Vec<String> = Vec::new();
    let (w, h) = (32u32, 32u32);

    // ---- FULL coverage: resolve of 4 identical samples == the exact flat colour, every pixel. ----
    checks += 1;
    match msaa_render_resolve(exec, w, h, MSAA_FULL_WGSL, 4) {
        Ok(plane) => {
            let bad = plane.chunks_exact(4).enumerate().find(|(_, p)| {
                (0..4).any(|k| (p[k] as i16 - MSAA_FULL_EXPECT[k] as i16).abs() > 1)
            });
            if let Some((i, p)) = bad {
                fails.push(format!(
                    "MSAA full-coverage resolve: texel {i} = {:?} but analytic expected {:?} (±1) — a \
                     fully-covered 4× MSAA target must resolve to the flat draw colour exactly",
                    &p[..4], MSAA_FULL_EXPECT
                ));
            }
        }
        Err(e) => fails.push(format!("MSAA full-coverage render+resolve errored: {e:?}")),
    }

    // ---- HALF coverage: exact fg interior, exact bg exterior, averaged-gray edge pixels. ----
    checks += 1;
    match msaa_render_resolve(exec, w, h, MSAA_HALF_WGSL, 4) {
        Ok(msaa) => {
            let at = |x: u32, y: u32| {
                let i = ((y * w + x) * 4) as usize;
                [msaa[i], msaa[i + 1], msaa[i + 2], msaa[i + 3]]
            };
            let is_fg = |p: [u8; 4]| (0..3).all(|k| p[k] >= 254);
            let is_bg = |p: [u8; 4]| (0..3).all(|k| p[k] <= 1);
            // Deep interior (lower-left, fy >> fx) fully covered → exact fg; deep exterior exact bg.
            let interior = at(2, h - 3);
            let exterior = at(w - 3, 2);
            if !is_fg(interior) {
                fails.push(format!(
                    "MSAA half-coverage interior {interior:?} must be exact fg (white)"
                ));
            }
            if !is_bg(exterior) {
                fails.push(format!(
                    "MSAA half-coverage exterior {exterior:?} must be exact bg (black)"
                ));
            }
            // The diagonal edge must carry averaged (neither pure-fg nor pure-bg) pixels — sub-pixel
            // coverage the hard rasterizer cannot produce. Every such pixel must lie ON the diagonal.
            let mut intermediates = 0u32;
            let mut max_off_diag = 0i64;
            for y in 0..h {
                for x in 0..w {
                    let p = at(x, y);
                    if !is_fg(p) && !is_bg(p) {
                        intermediates += 1;
                        max_off_diag = max_off_diag.max((x as i64 - y as i64).abs());
                    }
                }
            }
            if intermediates < w / 2 {
                fails.push(format!(
                    "MSAA half-coverage must antialias the diagonal — expected many averaged edge pixels, got \
                     {intermediates}"
                ));
            }
            if max_off_diag > 2 {
                fails.push(format!(
                    "every MSAA intermediate pixel must lie ON the diagonal edge (|x-y| <= 2), max off-diagonal \
                     was {max_off_diag}"
                ));
            }
            // Control: the SAME geometry at 1× is hard-aliased — ZERO intermediate pixels.
            checks += 1;
            match msaa_render_resolve(exec, w, h, MSAA_HALF_WGSL, 1) {
                Ok(noaa) => {
                    let inter_1x = noaa
                        .chunks_exact(4)
                        .filter(|p| !((0..3).all(|k| p[k] >= 254)) && !((0..3).all(|k| p[k] <= 1)))
                        .count();
                    if inter_1x != 0 {
                        fails.push(format!(
                            "the 1× control must be hard-aliased (0 intermediate pixels), got {inter_1x} — the \
                             gray edge is caused by MSAA averaging, not the geometry"
                        ));
                    }
                }
                Err(e) => fails.push(format!("MSAA 1× control render errored: {e:?}")),
            }
        }
        Err(e) => fails.push(format!("MSAA half-coverage render+resolve errored: {e:?}")),
    }

    (checks, fails)
}
