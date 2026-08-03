use super::*;
use crate::model::glconst::MAX_TEXTURE_UNITS;
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

/// Materialize the destination image used by a deferred `glCopyTex*` operation. This deliberately uses
/// the ordinary sampled-texture residency key: a later draw binds the exact object written by the GPU
/// copy and therefore does not replace it with a stale CPU-shadow upload.
pub(super) fn prepare_copy_destination(
    ctx: &mut GlContext,
    name: u32,
    cube: bool,
    cmds: &mut Vec<Cmd>,
) -> Option<(u32, TextureFormat)> {
    let texture = ctx.textures.get(name)?.clone();
    let uses_mipmaps = texture.uses_mipmaps();
    let generation = texture.sampled_generation();
    let (texture_ir, upload) = ctx
        .sampled_texture_ir(name, (generation.0, generation.1, uses_mipmaps))
        .ok()?;
    let format = sampled_format(texture.ir_format);
    if !upload {
        return Some((texture_ir, format));
    }
    let levels = texture.sampled_levels(uses_mipmaps);
    let (base_w, base_h, _) = levels.first()?.clone();
    let layers = if cube { 6 } else { 1 };
    cmds.push(Cmd::CreateTexture(
        texture_ir,
        TextureDesc {
            width: base_w as u32,
            height: base_h as u32,
            depth: layers,
            mip_levels: levels.len() as u32,
            sample_count: 1,
            dim: if cube { TextureDim::Cube } else { TextureDim::D2 },
            format,
            // BlitTexture performs format conversion through a render pass, so its destination must be
            // both copy-addressable and color-renderable. COPY_SRC keeps a copied image usable as a later
            // framebuffer-copy source without allocating a second backing texture.
            usage: texture_usage::SAMPLED
                | texture_usage::COPY_DST
                | texture_usage::COPY_SRC
                | if cube { 0 } else { texture_usage::RENDER_TARGET },
            label: "gl-copytex-destination".into(),
        },
    ));
    let texel = format.bytes_per_texel().unwrap_or(4) as u32;
    let mut copies = Vec::new();
    for (mip, (width, height, level_base)) in levels.iter().enumerate() {
        for layer in 0..layers {
            let data = if cube {
                if mip == 0 {
                    texture.cube_faces()[layer as usize]
                        .as_ref()
                        .unwrap_or(level_base)
                } else {
                    texture
                        .mip_chain()
                        .get(mip - 1)
                        .and_then(|level| {
                            if layer == 0 { Some(&level.data) } else { level.layers.get(layer as usize - 1) }
                        })
                        .unwrap_or(level_base)
                }
            } else {
                level_base
            };
            let stage = ctx.alloc_buffer_ir().ok()?;
            cmds.push(Cmd::CreateBuffer(
                stage,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::WriteBuffer { id: stage, offset: 0, data: data.as_ref().clone() });
            copies.push(Enc::CopyBufferToTextureRegion {
                src: stage,
                src_offset: 0,
                bytes_per_row: *width as u32 * texel,
                rows_per_image: *height as u32,
                dst: texture_ir,
                dst_sub: TextureSubresource { mip: mip as u32, layer: 0, aspect: hl_gpu::protocol::model::enums::TextureAspect::All },
                dst_origin: Origin3d { x: 0, y: 0, z: layer },
                extent: Extent3d { width: *width as u32, height: *height as u32, depth: 1 },
            });
        }
    }
    cmds.push(Cmd::Submit(CommandBuffer { encoder: copies, signal: None }));
    Some((texture_ir, format))
}

#[cfg(test)]
mod tests {
    use super::{SnapshotKey, SnapshotSource, TextureFormat};
    use std::sync::Arc;

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

    #[test]
    fn pixel_snapshot_identity_cannot_alias_while_both_planes_are_live() {
        let first = Arc::new(vec![0x11; 256]);
        let second = Arc::new(vec![0x4c; 256]);
        let key = |data: &Arc<Vec<u8>>| SnapshotKey {
            source: SnapshotSource::Pixels {
                data: Arc::as_ptr(data) as usize,
            },
            width: 8,
            height: 8,
            format: TextureFormat::Rgba8Unorm.to_u32(),
        };

        assert_ne!(
            key(&first),
            key(&second),
            "two retained planes cannot share an allocation address while both Arcs are live"
        );
        assert_eq!(
            key(&first),
            key(&Arc::clone(&first)),
            "two snapshots of the same immutable plane should share one upload"
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
    /// Staging buffers for the mip levels ABOVE the base, as `(buffer, level, width, height, layer)`. EMPTY for
    /// the single-level textures that are the overwhelming majority, so their lowering is unchanged.
    pub(super) mip_stages: Vec<(u32, u32, u32, u32, u32)>,
    /// Additional base-level cube-face staging buffers as `(buffer, layer)`; layer zero uses `stage_ir`.
    pub(super) layer_stages: Vec<(u32, u32)>,
    /// Bytes per texel of the staged plane. Four for every normalized shadow; one or two for an integer
    /// format whose texels are stored raw at their own channel count. The upload's row pitch is derived
    /// from this rather than assuming RGBA8, which would read an `R8Uint` plane four times too wide.
    pub(super) bytes_per_texel: u32,
    /// Array layers receiving the staged base image. Cube textures need six initialized faces even when
    /// the current GL model has only one canonical CPU plane.
    pub(super) layers: u32,
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
        let texture_dim = match prog.samp_types.get(declaration).map(String::as_str) {
            Some("samplerCube" | "isamplerCube" | "usamplerCube") => TextureDim::Cube,
            Some("sampler3D" | "isampler3D" | "usampler3D") => TextureDim::D3,
            _ => TextureDim::D2,
        };
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
                .filter(|unit| (0..MAX_TEXTURE_UNITS as i32).contains(unit))
                .unwrap_or(0) as usize;
            let (gl_tex, texture_generation, texture_swizzle) =
                if texture_dim == TextureDim::Cube {
                    (
                        d.cube_tex_units[unit],
                        d.cube_tex_generations[unit],
                        d.cube_tex_swizzles[unit],
                    )
                } else {
                    (
                        u64::from(d.tex_units[unit]),
                        d.tex_generations[unit],
                        d.tex_swizzles[unit],
                    )
                };
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
                        .get_internal(gl_tex)
                        .filter(|texture| texture.gen == texture_generation)
                        .cloned()
                });
            let shared_advanced = texture
                .as_mut()
                .is_some_and(crate::model::texture::GlTexture::resolve_shared);
            // Cross-pass FBO sampling: if this sampled GL texture is the color attachment an earlier render pass
            // rendered into, bind THAT render-target texture (the rendered pixels) directly — no staging upload
            // (its CPU plane is the pre-render zero storage). `stage_ir == 0` marks the copy-free bind.
            if let Some(&rt_ir) = u32::try_from(gl_tex)
                .ok()
                .and_then(|gl_tex| fbo_tex_ir.get(&(gl_tex, texture_generation)))
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
                    swizzle: texture_swizzle,
                    mip_stages: Vec::new(),
                    layer_stages: Vec::new(),
                    bytes_per_texel: 4,
                    layers: 1,
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
                    .or_else(|| {
                        u32::try_from(gl_tex).ok().and_then(|gl_tex| {
                            ctx.resident_fbo_target_tex(gl_tex, texture_generation)
                        })
                    })
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
                            swizzle: texture_swizzle,
                            mip_stages: Vec::new(),
                            layer_stages: Vec::new(),
                            bytes_per_texel: 4,
                            layers: 1,
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
                    // placeholder texture + a default sampler (created ONCE, cached on the context) at this
                    // sampler's host binding so the driver covers every declared sampler; the executor's
                    // used-binding filter then trims to the shader's actually-sampled subset.
                    //
                    // The placeholder texel is OPAQUE black. ES 3.0 §3.8.2 fixes what an INCOMPLETE texture
                    // samples as — (0, 0, 0, 1) — and this placeholder is what an app hits after deleting a
                    // bound texture. A transparent (0,0,0,0) texel turned that geometry invisible instead of
                    // black, which is a much harder thing to notice than a black quad.
                    let (tex_ir, samp_ir, create_texture, create_sampler) =
                        ctx.default_placeholder(texture_dim)?;
                    let stage_ir = if create_texture {
                        let stage_ir = ctx.alloc_buffer_ir()?;
                        cmds.push(Cmd::CreateTexture(
                            tex_ir,
                            TextureDesc {
                                width: 1,
                                height: 1,
                                depth: if texture_dim == TextureDim::Cube {
                                    6
                                } else {
                                    1
                                },
                                mip_levels: 1,
                                sample_count: 1,
                                dim: texture_dim,
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
                            data: vec![0, 0, 0, 0xff],
                        });
                        stage_ir
                    } else {
                        0
                    };
                    if create_sampler {
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
                    }
                    texbinds.push(TexBind {
                        slot,
                        tex_ir,
                        samp_ir,
                        stage_ir,
                        w: 1,
                        h: 1,
                        sampler: sampler_name(name, element, elements),
                        flip_y: false,
                        swizzle: texture_swizzle,
                        mip_stages: Vec::new(),
                        // A cube placeholder is one opaque-black texel repeated across all six faces.
                        // Reusing the same staging buffer is sufficient, but every destination layer
                        // needs an explicit copy; initializing only +X leaves the other five faces
                        // undefined and makes empty/incomplete cubes transparent or nondeterministic.
                        layer_stages: if texture_dim == TextureDim::Cube && stage_ir != 0 {
                            (1..6).map(|layer| (stage_ir, layer)).collect()
                        } else {
                            Vec::new()
                        },
                        bytes_per_texel: 4,
                        layers: if texture_dim == TextureDim::Cube {
                            6
                        } else {
                            1
                        },
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
                .get_internal(gl_tex)
                .is_some_and(|texture| texture.gen == t.gen);
            let upload_format = sampled_format(t.ir_format);
            let sampler = Pipeline::sampler_desc(&t, &d.samp_objs[unit]);
            let uses_mipmaps = d.samp_objs[unit]
                .as_ref()
                .map_or_else(|| t.uses_mipmaps(), |sampler| sampler.uses_mipmaps());
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
            // The arm label is produced BY the match rather than re-derived from the same predicates
            // afterwards: a second copy of this condition could disagree with the branch actually taken,
            // and the whole point of the label is to be trusted about which one ran.
            let (tex_ir, needs_upload, ephemeral, arm) = match snapshot
                .as_ref()
                .filter(|_| !shared_advanced)
                .and_then(|snapshot| snapshot.sampled_ir)
                .filter(|_| uses_mipmaps == t.uses_mipmaps())
            {
                Some(texture) => (texture, false, false, "snapshot-sampled"),
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
                    (texture, upload, false, "shared-residency")
                }
                None if live_generation && u32::try_from(gl_tex).is_ok() => {
                    let generation = t.sampled_generation();
                    let (texture, upload) = ctx.sampled_texture_ir(
                        gl_tex as u32,
                        (generation.0, generation.1, uses_mipmaps),
                    )?;
                    (texture, upload, false, "live-generation")
                }
                None => match snapshot_key.and_then(|key| snapshots.get(&key).copied()) {
                    Some(texture) => (texture, false, true, "snapshot-cache"),
                    None => {
                        let texture = ctx.alloc_texture_ir()?;
                        if let Some(key) = snapshot_key {
                            snapshots.insert(key, texture);
                        }
                        (texture, true, true, "fresh-allocation")
                    }
                },
            };
            let _ = arm;
            let (samp_ir, create_sampler) = ctx.sampler_ir(&sampler)?;
            // The levels the host texture receives, from the EFFECTIVE base downwards — GL's
            // `[BASE_LEVEL, MAX_LEVEL]` window, which re-indexes the pyramid so the base level becomes the
            // host's level 0. Every declared level must be uploaded, so the window and the level count are
            // taken from one place.
            // GL defines sampling an incomplete texture as opaque black. A host cube view cannot itself
            // represent missing/mismatched faces or holes in a mip pyramid, so materialize a complete
            // 1x1 black cube instead of silently copying a surviving face into every invalid slot.
            let incomplete_cube = texture_dim == TextureDim::Cube && !t.cube_complete(uses_mipmaps);
            let cube_fallback = incomplete_cube.then(|| Arc::new(vec![0, 0, 0, 0xff]));
            let levels = cube_fallback.as_ref().map_or_else(
                || t.sampled_levels(uses_mipmaps),
                |pixels| vec![(1, 1, Arc::clone(pixels))],
            );
            let mip_levels = levels.len().max(1) as u32;
            let (base_w, base_h, base_data) =
                levels
                    .first()
                    .cloned()
                    .unwrap_or((t.w, t.h, Arc::clone(&t.data)));
            let mut mip_stages: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
            let mut layer_stages: Vec<(u32, u32)> = Vec::new();
            // INSTRUMENTED BUILDS ONLY. Both branches, so "no staging write was emitted" can be told
            // apart from "this lowering path never ran". The first is a defect; the second means the
            // trace is in the wrong place, and only reporting both distinguishes them.
            #[cfg(feature = "verbose")]
            {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static BINDS: AtomicUsize = AtomicUsize::new(0);
                let sentinel_upload = base_data
                    .get(..8)
                    .is_some_and(|head| head == [0x11, 0x22, 0x33, 0xff, 0x11, 0x22, 0x33, 0xff]);
                let extension_upload = base_data.starts_with(b"L_OE");
                if BINDS.fetch_add(1, Ordering::Relaxed) < 12 || sentinel_upload || extension_upload
                {
                    // Whether this GL name is actually a RENDER TARGET, read off the attachment table
                    // rather than inferred from its content being absent and its extent looking like a
                    // canvas. `recorded_framebuffers` is the set of FBOs this frame drew into, so a hit
                    // means the texture was rendered into and then sampled -- the case whose content
                    // never passes through an upload and so never reaches the CPU shadow. A miss moves
                    // the defect somewhere else entirely and must not be quietly read as a hit.
                    let mut drawn_into: Vec<u32> = ctx
                        .recorded_framebuffers()
                        .filter(|fbo| *fbo != 0 && ctx.framebuffer_color_attachment(*fbo) == gl_tex)
                        .collect();
                    drawn_into.sort_unstable();
                    drawn_into.dedup();
                    // The inputs the arm was chosen FROM, beside the arm. `needs_upload=true` has three
                    // different causes wearing one symptom -- a shadow that is stale, one that was never
                    // written, and a path that never consults residency at all -- and only the arm plus
                    // these inputs tell them apart. `shadow_nonzero` is the base rate in miniature: a
                    // zero shadow means nothing without a texture in the same run that has one.
                    hl_log::hl_error!(
                        hl_log::tag::GL,
                        "texbind gl_tex={gl_tex} texture={tex_ir} arm={arm} \
                         needs_upload={needs_upload} ephemeral={ephemeral} \
                         shadow_bytes={} shadow_nonzero={} generation={:?} shared_residency={} \
                         live_generation={live_generation} shared_advanced={shared_advanced} \
                         snapshot={} render_target_of={:?} sentinel_upload={sentinel_upload} \
                         extension_upload={extension_upload} {}x{}",
                        base_data.len(),
                        base_data.iter().any(|byte| *byte != 0),
                        t.sampled_generation(),
                        t.shared_residency().is_some(),
                        snapshot.is_some(),
                        drawn_into,
                        base_w,
                        base_h
                    );
                }
            }
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
                        width: base_w as u32,
                        height: base_h as u32,
                        depth: if texture_dim == TextureDim::Cube {
                            6
                        } else {
                            t.depth.max(1) as u32
                        },
                        mip_levels,
                        sample_count: 1,
                        dim: texture_dim,
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
                        size: base_data.len() as u64,
                        usage: buffer_usage::COPY_SRC,
                        label: String::new(),
                    },
                ));
                // INSTRUMENTED BUILDS ONLY. The bytes as they enter the IR, which is the cut between a
                // guest-side substitution (wrong here) and transport or host replay (right here, wrong
                // on readback). The shim is already known to read the application's bytes correctly, so
                // this is the next boundary downstream and the two answers want different owners.
                #[cfg(feature = "verbose")]
                {
                    let traced: Option<usize> = std::env::var("HL_GL_UPLOAD_TRACE_SPAN")
                        .ok()
                        .and_then(|value| value.parse().ok());
                    // Also the first few unconditionally. A filtered trace that prints nothing cannot
                    // distinguish "no staging write happened for this texture" from "the length I
                    // filtered on was wrong", and those are opposite findings.
                    use std::sync::atomic::{AtomicUsize, Ordering};
                    static STAGED: AtomicUsize = AtomicUsize::new(0);
                    let count = STAGED.fetch_add(1, Ordering::Relaxed);
                    if traced == Some(base_data.len()) || count < 12 {
                        let head: Vec<String> = base_data
                            .iter()
                            .take(8)
                            .map(|b| format!("{b:02x}"))
                            .collect();
                        hl_log::hl_error!(
                            hl_log::tag::GL,
                            "encode stage buffer={stage_ir} texture={tex_ir} {}x{} bytes={} head=[{}]",
                            base_w,
                            base_h,
                            base_data.len(),
                            head.join(" ")
                        );
                    }
                }
                cmds.push(Cmd::WriteBuffer {
                    id: stage_ir,
                    offset: 0,
                    data: if incomplete_cube {
                        cube_fallback
                            .as_ref()
                            .expect("incomplete cube has fallback")
                            .as_ref()
                            .clone()
                    } else if texture_dim == TextureDim::Cube {
                        t.cube_faces()[0]
                            .as_ref()
                            .unwrap_or(&base_data)
                            .as_ref()
                            .clone()
                    } else {
                        (*base_data).clone()
                    },
                });
                if texture_dim == TextureDim::Cube {
                    for (layer, face) in t.cube_faces().iter().enumerate().skip(1) {
                        let face_ir = ctx.alloc_buffer_ir()?;
                        let data = if incomplete_cube {
                            cube_fallback
                                .as_ref()
                                .expect("incomplete cube has fallback")
                        } else {
                            face.as_ref().unwrap_or(&base_data)
                        };
                        cmds.push(Cmd::CreateBuffer(
                            face_ir,
                            BufferDesc {
                                size: data.len() as u64,
                                usage: buffer_usage::COPY_SRC,
                                label: String::new(),
                            },
                        ));
                        cmds.push(Cmd::WriteBuffer {
                            id: face_ir,
                            offset: 0,
                            data: data.as_ref().clone(),
                        });
                        layer_stages.push((face_ir, layer as u32));
                    }
                } else {
                    for (layer, data) in t.layers().iter().enumerate() {
                        let layer_ir = ctx.alloc_buffer_ir()?;
                        cmds.push(Cmd::CreateBuffer(
                            layer_ir,
                            BufferDesc {
                                size: data.len() as u64,
                                usage: buffer_usage::COPY_SRC,
                                label: String::new(),
                            },
                        ));
                        cmds.push(Cmd::WriteBuffer {
                            id: layer_ir,
                            offset: 0,
                            data: data.as_ref().clone(),
                        });
                        layer_stages.push((layer_ir, layer as u32 + 1));
                    }
                }
                // One staging buffer per declared level above the base. A host texture must have every
                // level it declares, so this walks exactly the levels `mip_levels` counted.
                for (index, (lw, lh, data)) in levels.iter().enumerate().skip(1) {
                    let level_ir = ctx.alloc_buffer_ir()?;
                    cmds.push(Cmd::CreateBuffer(
                        level_ir,
                        BufferDesc {
                            size: data.len() as u64,
                            usage: buffer_usage::COPY_SRC,
                            label: String::new(),
                        },
                    ));
                    cmds.push(Cmd::WriteBuffer {
                        id: level_ir,
                        offset: 0,
                        data: (**data).clone(),
                    });
                    mip_stages.push((level_ir, index as u32, *lw as u32, *lh as u32, 0));
                    if t.depth > 1 || texture_dim == TextureDim::Cube {
                        if let Some(level) = t.mip_chain().get(index - 1) {
                            for (layer, data) in level.layers.iter().enumerate() {
                                let layer_ir = ctx.alloc_buffer_ir()?;
                                cmds.push(Cmd::CreateBuffer(
                                    layer_ir,
                                    BufferDesc {
                                        size: data.len() as u64,
                                        usage: buffer_usage::COPY_SRC,
                                        label: String::new(),
                                    },
                                ));
                                cmds.push(Cmd::WriteBuffer {
                                    id: layer_ir,
                                    offset: 0,
                                    data: data.as_ref().clone(),
                                });
                                mip_stages.push((
                                    layer_ir,
                                    index as u32,
                                    *lw as u32,
                                    *lh as u32,
                                    layer as u32 + 1,
                                ));
                            }
                        }
                    }
                }
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
                w: base_w as u32,
                h: base_h as u32,
                sampler: sampler_name(name, element, elements),
                flip_y: false,
                swizzle: texture_swizzle,
                mip_stages,
                layer_stages,
                bytes_per_texel: upload_format.bytes_per_texel().unwrap_or(4) as u32,
                layers: if texture_dim == TextureDim::Cube {
                    6
                } else {
                    t.depth.max(1) as u32
                },
            });
        }
        sampler_base += elements as usize;
    }

    Ok(texbinds)
}

/// The format a sampled CPU shadow is materialized as.
///
/// The GL model keeps shadows as canonical RGBA8 whatever the declared internal format, so this collapses
/// to `Rgba8Unorm` (or its sRGB sibling). The INTEGER formats are the exception and pass through
/// unchanged: their texels are raw integers with no normalized reading, the plane really is one/two/four
/// bytes per texel rather than always four, and the shader reads them through an integer sample type that
/// a unorm format cannot satisfy.
fn sampled_format(format: TextureFormat) -> TextureFormat {
    match format {
        TextureFormat::Rgba8Srgb | TextureFormat::Bgra8Srgb => TextureFormat::Rgba8Srgb,
        format if is_integer_format(format) => format,
        _ => TextureFormat::Rgba8Unorm,
    }
}

/// Whether this IR format carries raw integer texels (the `INTEGER_FORMATS` capability set).
pub(super) fn is_integer_format(format: TextureFormat) -> bool {
    matches!(
        format,
        TextureFormat::Rgba8Uint
            | TextureFormat::Rgba8Sint
            | TextureFormat::R8Uint
            | TextureFormat::R8Sint
            | TextureFormat::Rg8Uint
            | TextureFormat::Rg8Sint
            | TextureFormat::Rgba32Uint
            | TextureFormat::Rgba32Sint
    )
}

fn sampler_name(name: &str, element: u32, elements: u32) -> String {
    if elements == 1 {
        name.to_owned()
    } else {
        format!("{name}[{element}]")
    }
}
