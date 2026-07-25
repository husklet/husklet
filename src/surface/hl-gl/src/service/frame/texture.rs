use super::*;
use crate::model::program::Program;

pub(super) struct TexBind {
    pub(super) slot: usize,
    pub(super) tex_ir: u32,
    pub(super) samp_ir: u32,
    pub(super) stage_ir: u32,
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) sampler: String,
    pub(super) flip_y: bool,
}

pub(super) fn lower_textures(
    ctx: &mut GlContext,
    d: &DrawCall,
    prog: &Program,
    cmds: &mut Vec<Cmd>,
    fbo_tex_ir: &std::collections::HashMap<(u32, u64), u32>,
) -> Vec<TexBind> {
    // ---- sampler-bound textures ----
    let mut texbinds: Vec<TexBind> = Vec::new();
    for i in 0..prog.samp_names.len().min(4) {
        // `i` indexes the driver's sampler reflection (the `glUniform1i` sampler→unit map); `slot` is the
        // sampler's HOST binding index, which the verbatim path remaps away from `i`.
        let slot = prog.samp_bindings.get(i).copied().unwrap_or(i as u32) as usize;
        let unit = if (0..8).contains(&prog.samp_units[i]) {
            prog.samp_units[i] as usize
        } else {
            i
        };
        let gl_tex = d.tex_units[unit];
        // Cross-pass FBO sampling: if this sampled GL texture is the color attachment an earlier render pass
        // rendered into, bind THAT render-target texture (the rendered pixels) directly — no staging upload
        // (its CPU plane is the pre-render zero storage). `stage_ir == 0` marks the copy-free bind.
        let texture_generation = d.tex_generations[unit];
        if let Some(&rt_ir) = fbo_tex_ir.get(&(gl_tex, texture_generation)) {
            let t = match ctx.textures.get(gl_tex) {
                Some(t) => t.clone(),
                None => continue,
            };
            let samp_ir = ctx.alloc_sampler_ir();
            cmds.push(Cmd::CreateSampler(
                samp_ir,
                Pipeline::sampler_desc(&t, &d.samp_objs[unit]),
            ));
            texbinds.push(TexBind {
                slot,
                tex_ir: rt_ir,
                samp_ir,
                stage_ir: 0,
                w: t.w as u32,
                h: t.h as u32,
                sampler: prog.samp_names[i].clone(),
                flip_y: true,
            });
            continue;
        }
        // Cross-FRAME FBO sampling: if this GL texture holds no REAL uploaded pixels but IS the color
        // attachment a PRIOR frame's offscreen pass rendered into (e.g. a `glFlush`/`glFinish` executed the
        // tile passes into their persistent render targets — see `crate::service::swap::flush_offscreen`),
        // bind THAT resident render-target texture (its rendered pixels) rather than its CPU plane. The gate
        // is `!has_real_pixels()`, NOT `!has_data()`: an FBO color attachment allocated via
        // `glTexImage2D(…, NULL)` / `glTexStorage2D` carries a ZEROED plane (so `has_data()` is TRUE) whose
        // real content lives only in the render target. Sampling the zeroed plane instead of the rendered
        // tile was exactly why Chrome's tile→window composite read back blank. An ordinary sampled texture
        // (a real `glTexImage2D`/`glTexSubImage2D` upload) has `has_real_pixels()` TRUE, so it is never
        // diverted — single-frame apps stay byte-identical, and a NULL-allocated texture that was never an
        // FBO target falls through to the upload arm below (uploading its zeroed plane) exactly as before.
        let render_target_only = ctx
            .textures
            .get(gl_tex)
            .map(|t| !t.has_real_pixels())
            .unwrap_or(false);
        if render_target_only {
            if let Some(rt_ir) = ctx.resident_fbo_target_tex(gl_tex, texture_generation) {
                if let Some(t) = ctx.textures.get(gl_tex).cloned() {
                    let samp_ir = ctx.alloc_sampler_ir();
                    cmds.push(Cmd::CreateSampler(
                        samp_ir,
                        Pipeline::sampler_desc(&t, &d.samp_objs[unit]),
                    ));
                    texbinds.push(TexBind {
                        slot,
                        tex_ir: rt_ir,
                        samp_ir,
                        stage_ir: 0,
                        w: t.w as u32,
                        h: t.h as u32,
                        sampler: prog.samp_names[i].clone(),
                        flip_y: true,
                    });
                    continue;
                }
            }
        }
        let t = match ctx.textures.get(gl_tex) {
            Some(t) if t.has_data() => t.clone(),
            _ => {
                // The compiled shader DECLARES + samples this sampler (so wgpu's auto bind-group layout
                // carries its texture+sampler bindings), but this draw has no real GL texture with uploaded
                // pixels bound at its unit (an unbound unit, or a texture object with no storage yet). NEVER
                // skip it: a skipped declared sampler leaves the bind group short of the layout and
                // `create_bind_group` NACKs ("bindings (3) does not match (7)"). Bind a shared 1x1
                // transparent-black placeholder texture + a default sampler (created ONCE, cached on the
                // context) at this sampler's host binding so the driver covers every declared sampler; the
                // executor's used-binding filter then trims to the shader's actually-sampled subset.
                let (tex_ir, samp_ir, needs_create) = ctx.default_placeholder();
                let stage_ir = if needs_create {
                    let stage_ir = ctx.alloc_buffer_ir();
                    cmds.push(Cmd::CreateTexture(
                        tex_ir,
                        TextureDesc {
                            width: 1,
                            height: 1,
                            depth: 1,
                            mip_levels: 1,
                            sample_count: 1,
                            dim: TextureDim::D2,
                            format: TextureFormat::Rgba8Unorm,
                            usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                            label: "gl-placeholder-tex".into(),
                        },
                    ));
                    cmds.push(Cmd::CreateBuffer(
                        stage_ir,
                        BufferDesc {
                            size: 4,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ));
                    cmds.push(Cmd::WriteBuffer {
                        id: stage_ir,
                        offset: 0,
                        data: vec![0u8; 4],
                    });
                    cmds.push(Cmd::CreateSampler(
                        samp_ir,
                        SamplerDesc {
                            min_filter: Filter::Nearest,
                            mag_filter: Filter::Nearest,
                            mip_filter: Filter::Nearest,
                            address_u: AddressMode::ClampToEdge,
                            address_v: AddressMode::ClampToEdge,
                            address_w: AddressMode::ClampToEdge,
                        },
                    ));
                    stage_ir
                } else {
                    0
                };
                texbinds.push(TexBind {
                    slot,
                    tex_ir,
                    samp_ir,
                    stage_ir,
                    w: 1,
                    h: 1,
                    sampler: prog.samp_names[i].clone(),
                    flip_y: false,
                });
                continue;
            }
        };
        // Residency cache: a sampled texture (a GskGL glyph/mask atlas is bound across hundreds of draws
        // and re-used every frame) is `CreateTexture`d + staged + copied ONLY on first sight / content
        // change; later references reuse the resident IR id and upload nothing (`stage_ir == 0` marks the
        // copy-free bind, the same convention the cross-pass FBO sample uses).
        let (tex_ir, needs_upload) = ctx.sampled_texture_ir(gl_tex, t.gen);
        let samp_ir = ctx.alloc_sampler_ir();
        let stage_ir = if needs_upload {
            let stage_ir = ctx.alloc_buffer_ir();
            cmds.push(Cmd::CreateTexture(
                tex_ir,
                TextureDesc {
                    width: t.w as u32,
                    height: t.h as u32,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: t.ir_format,
                    usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::CreateBuffer(
                stage_ir,
                BufferDesc {
                    size: t.data.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id: stage_ir,
                offset: 0,
                data: t.data.clone(),
            });
            stage_ir
        } else {
            0
        };
        cmds.push(Cmd::CreateSampler(
            samp_ir,
            Pipeline::sampler_desc(&t, &d.samp_objs[unit]),
        ));
        texbinds.push(TexBind {
            slot,
            tex_ir,
            samp_ir,
            stage_ir,
            w: t.w as u32,
            h: t.h as u32,
            sampler: prog.samp_names[i].clone(),
            flip_y: false,
        });
    }

    texbinds
}
