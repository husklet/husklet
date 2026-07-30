use super::*;
use crate::model::program::Program;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SnapshotSource {
    Shared { storage: u64, revision: u64 },
    Pixels { data: usize },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct SnapshotKey {
    source: SnapshotSource,
    width: u32,
    height: u32,
    format: u32,
}

pub(super) type SnapshotTextures = std::collections::HashMap<SnapshotKey, u32>;

#[cfg(test)]
mod tests {
    use super::{SnapshotKey, SnapshotSource, TextureFormat};

    #[test]
    fn snapshot_identity_distinguishes_storage_and_revision() {
        let key = SnapshotKey {
            source: SnapshotSource::Shared {
                storage: 11,
                revision: 13,
            },
            width: 17,
            height: 19,
            format: TextureFormat::Rgba8Unorm.to_u32(),
        };
        assert_ne!(
            key,
            SnapshotKey {
                source: SnapshotSource::Shared {
                    storage: 12,
                    revision: 13,
                },
                ..key
            }
        );
        assert_ne!(
            key,
            SnapshotKey {
                source: SnapshotSource::Shared {
                    storage: 11,
                    revision: 14,
                },
                ..key
            }
        );
    }
}

pub(super) struct TexBind {
    pub(super) slot: usize,
    pub(super) tex_ir: u32,
    pub(super) samp_ir: u32,
    pub(super) stage_ir: u32,
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) sampler: String,
    pub(super) flip_y: bool,
    pub(super) swizzle: [u32; 4],
}

pub(super) fn lower_textures(
    ctx: &mut GlContext,
    d: &DrawCall,
    prog: &Program,
    cmds: &mut Vec<Cmd>,
    fbo_tex_ir: &std::collections::HashMap<(u32, u64), u32>,
    snapshots: &mut SnapshotTextures,
) -> hl_gpu::Result<Vec<TexBind>> {
    // ---- sampler-bound textures ----
    let mut texbinds: Vec<TexBind> = Vec::new();
    let mut sampler_base = 0usize;
    for (declaration, name) in prog.samp_names.iter().enumerate() {
        let elements = prog
            .samp_arrays
            .get(declaration)
            .copied()
            .unwrap_or(1)
            .max(1);
        for element in 0..elements {
            let i = sampler_base + element as usize;
            // `i` indexes the driver's sampler reflection (the `glUniform1i` sampler→unit map); `slot` is the
            // sampler's HOST binding index, which the verbatim path remaps away from `i`.
            let slot = prog.samp_bindings.get(i).copied().unwrap_or(i as u32) as usize;
            let unit = d
                .samp_units
                .get(i)
                .copied()
                .filter(|unit| (0..8).contains(unit))
                .unwrap_or(0) as usize;
            let gl_tex = d.tex_units[unit];
            let texture_generation = d.tex_generations[unit];
            let snapshot = d
                .textures
                .iter()
                .find(|snapshot| {
                    snapshot.name == gl_tex && snapshot.generation == texture_generation
                })
                .cloned();
            let mut texture = snapshot
                .as_ref()
                .map(|snapshot| snapshot.texture.clone())
                .or_else(|| {
                    ctx.textures
                        .get(gl_tex)
                        .filter(|texture| texture.gen == texture_generation)
                        .cloned()
                });
            let shared_advanced = texture
                .as_mut()
                .is_some_and(crate::model::texture::GlTexture::resolve_shared);
            // Cross-pass FBO sampling: if this sampled GL texture is the color attachment an earlier render pass
            // rendered into, bind THAT render-target texture (the rendered pixels) directly — no staging upload
            // (its CPU plane is the pre-render zero storage). `stage_ir == 0` marks the copy-free bind.
            if let Some(&rt_ir) = (gl_tex != 0)
                .then(|| fbo_tex_ir.get(&(gl_tex, texture_generation)))
                .flatten()
            {
                let (sampler, width, height) = match texture.as_ref() {
                    Some(texture) => (
                        Pipeline::sampler_desc(texture, &d.samp_objs[unit]),
                        texture.w as u32,
                        texture.h as u32,
                    ),
                    None => (
                        SamplerDesc {
                            min_filter: Filter::Nearest,
                            mag_filter: Filter::Nearest,
                            mip_filter: Filter::Nearest,
                            address_u: AddressMode::ClampToEdge,
                            address_v: AddressMode::ClampToEdge,
                            address_w: AddressMode::ClampToEdge,
                            ..SamplerDesc::default()
                        },
                        1,
                        1,
                    ),
                };
                let (samp_ir, create_sampler) = ctx.sampler_ir(&sampler)?;
                if create_sampler {
                    cmds.push(Cmd::CreateSampler(samp_ir, sampler));
                }
                texbinds.push(TexBind {
                    slot,
                    tex_ir: rt_ir,
                    samp_ir,
                    stage_ir: 0,
                    w: width,
                    h: height,
                    sampler: sampler_name(name, element, elements),
                    flip_y: true,
                    swizzle: d.tex_swizzles[unit],
                });
                continue;
            }
            // Cross-FRAME FBO sampling: when a prior accepted pass was the latest writer, bind its persistent
            // render target rather than the CPU shadow. GL exposes one texture image, but this backend splits it
            // into CPU-upload and GPU-render resources, so authority must follow the latest write. In particular,
            // a texture initialized by `glTexImage2D` and then rendered as an FBO is GPU-authoritative despite
            // still having real CPU pixels; a later `glTexSubImage2D` switches authority back to CPU.
            let render_target_current = texture
                .as_ref()
                .map(|t| t.gpu_authoritative() || !t.has_real_pixels())
                .unwrap_or(false);
            if render_target_current {
                if let Some(rt_ir) = snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.fbo_ir)
                    .or_else(|| ctx.resident_fbo_target_tex(gl_tex, texture_generation))
                {
                    if let Some(t) = texture.clone() {
                        let sampler = Pipeline::sampler_desc(&t, &d.samp_objs[unit]);
                        let (samp_ir, create_sampler) = ctx.sampler_ir(&sampler)?;
                        if create_sampler {
                            cmds.push(Cmd::CreateSampler(samp_ir, sampler));
                        }
                        texbinds.push(TexBind {
                            slot,
                            tex_ir: rt_ir,
                            samp_ir,
                            stage_ir: 0,
                            w: t.w as u32,
                            h: t.h as u32,
                            sampler: sampler_name(name, element, elements),
                            flip_y: true,
                            swizzle: d.tex_swizzles[unit],
                        });
                        continue;
                    }
                }
            }
            let t = match texture {
                Some(t) if t.has_data() => t,
                _ => {
                    // The compiled shader DECLARES + samples this sampler (so wgpu's auto bind-group layout
                    // carries its texture+sampler bindings), but this draw has no real GL texture with uploaded
                    // pixels bound at its unit (an unbound unit, or a texture object with no storage yet). NEVER
                    // skip it: a skipped declared sampler leaves the bind group short of the layout and
                    // `create_bind_group` NACKs ("bindings (3) does not match (7)"). Bind a shared 1x1
                    // transparent-black placeholder texture + a default sampler (created ONCE, cached on the
                    // context) at this sampler's host binding so the driver covers every declared sampler; the
                    // executor's used-binding filter then trims to the shader's actually-sampled subset.
                    let (tex_ir, samp_ir, needs_create) = ctx.default_placeholder()?;
                    let stage_ir = if needs_create {
                        let stage_ir = ctx.alloc_buffer_ir()?;
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
                                ..SamplerDesc::default()
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
                        sampler: sampler_name(name, element, elements),
                        flip_y: false,
                        swizzle: d.tex_swizzles[unit],
                    });
                    continue;
                }
            };
            // Live GL objects use the ordinary name/generation residency cache. Retained imported-image
            // snapshots use storage identity + CPU revision across frames. GPU render writes explicitly
            // invalidate that storage's entry after the accepted batch, so an unchanged revision is reusable
            // without allowing a newer GPU-produced image to alias its old CPU upload.
            let live_generation = ctx
                .textures
                .get(gl_tex)
                .is_some_and(|texture| texture.gen == t.gen);
            let upload_format = sampled_format(t.ir_format);
            let snapshot_key = snapshot.as_ref().map(|_| {
                let source = t.shared_identity().map_or_else(
                    || SnapshotSource::Pixels {
                        data: Arc::as_ptr(&t.data) as usize,
                    },
                    |(storage, revision)| SnapshotSource::Shared { storage, revision },
                );
                SnapshotKey {
                    source,
                    width: t.w as u32,
                    height: t.h as u32,
                    format: sampled_format(t.ir_format).to_u32(),
                }
            });
            let (tex_ir, needs_upload, ephemeral) = match snapshot
                .as_ref()
                .filter(|_| !shared_advanced)
                .and_then(|snapshot| snapshot.sampled_ir)
            {
                Some(texture) => (texture, false, false),
                None if t.shared_residency().is_some() => {
                    let (storage, revision, residency) = t.shared_residency().unwrap();
                    let (texture, upload) = ctx.shared_texture_ir(
                        (
                            storage,
                            revision,
                            t.w as u32,
                            t.h as u32,
                            upload_format.to_u32(),
                        ),
                        residency,
                    )?;
                    (texture, upload, false)
                }
                None if live_generation => {
                    let (texture, upload) =
                        ctx.sampled_texture_ir(gl_tex, t.sampled_generation())?;
                    (texture, upload, false)
                }
                None => match snapshot_key.and_then(|key| snapshots.get(&key).copied()) {
                    Some(texture) => (texture, false, true),
                    None => {
                        let texture = ctx.alloc_texture_ir()?;
                        if let Some(key) = snapshot_key {
                            snapshots.insert(key, texture);
                        }
                        (texture, true, true)
                    }
                },
            };
            let sampler = Pipeline::sampler_desc(&t, &d.samp_objs[unit]);
            let (samp_ir, create_sampler) = ctx.sampler_ir(&sampler)?;
            let stage_ir = if needs_upload {
                let stage_ir = ctx.alloc_buffer_ir()?;
                // The GL model deliberately keeps CPU texture shadows as canonical RGBA8 regardless of
                // their declared internal format. Preserve that representation at the upload boundary:
                // advertising R8/RG8/float storage here while copying four bytes per texel made wgpu read
                // channel bytes as adjacent texels (Chrome's glyph/mask atlases lost most pixels). Native
                // formats remain authoritative for render-target storage and resident FBO sampling; a CPU
                // shadow is a sampled RGBA image, with sRGB decoding retained when the internal format
                // requires it.
                cmds.push(Cmd::CreateTexture(
                    tex_ir,
                    TextureDesc {
                        width: t.w as u32,
                        height: t.h as u32,
                        depth: 1,
                        mip_levels: 1,
                        sample_count: 1,
                        dim: TextureDim::D2,
                        format: upload_format,
                        usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                        label: if ephemeral {
                            "gl-retired-snapshot".to_owned()
                        } else {
                            String::new()
                        },
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
                    data: (*t.data).clone(),
                });
                stage_ir
            } else {
                0
            };
            if create_sampler {
                cmds.push(Cmd::CreateSampler(samp_ir, sampler));
            }
            texbinds.push(TexBind {
                slot,
                tex_ir,
                samp_ir,
                stage_ir,
                w: t.w as u32,
                h: t.h as u32,
                sampler: sampler_name(name, element, elements),
                flip_y: false,
                swizzle: d.tex_swizzles[unit],
            });
        }
        sampler_base += elements as usize;
    }

    Ok(texbinds)
}

fn sampled_format(format: TextureFormat) -> TextureFormat {
    match format {
        TextureFormat::Rgba8Srgb | TextureFormat::Bgra8Srgb => TextureFormat::Rgba8Srgb,
        _ => TextureFormat::Rgba8Unorm,
    }
}

fn sampler_name(name: &str, element: u32, elements: u32) -> String {
    if elements == 1 {
        name.to_owned()
    } else {
        format!("{name}[{element}]")
    }
}
