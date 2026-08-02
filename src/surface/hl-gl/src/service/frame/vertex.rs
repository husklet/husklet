use super::*;
use crate::model::program::{Attr, Program};

pub(super) struct ClientSlot {
    pub(super) ir: u32,
    pub(super) bytes: usize,
    pub(super) stride: u32,
    pub(super) step_mode: u32,
    pub(super) location: u32,
    pub(super) format: u32,
}

pub(super) struct VertexLowering {
    pub(super) nslot: usize,
    pub(super) slot_stride: Vec<u32>,
    pub(super) slot_base: Vec<u32>,
    pub(super) slot_ir: Vec<u32>,
    pub(super) slot_bytes: Vec<usize>,
    pub(super) attr_slot: [i32; crate::model::program::MAX_ATTR],
    pub(super) nvd: usize,
    pub(super) client_slots: Vec<ClientSlot>,
    pub(super) expanded_indices: Option<Vec<u32>>,
    pub(super) index_ir: u32,
}

fn vertex_slot_bytes(
    draw: &DrawCall,
    attributes: &[Attr],
    stride: u32,
    base: u32,
) -> Option<usize> {
    let per_instance = attributes.iter().any(|attribute| attribute.divisor > 0);
    let end = if per_instance {
        draw.first_instance
            .checked_add(draw.instance_count.max(1))?
    } else if draw.indexed {
        return None;
    } else {
        u32::try_from(draw.first.max(0))
            .ok()?
            .checked_add(u32::try_from(draw.count.max(0)).ok()?)?
    };
    if end == 0 {
        return Some(base as usize);
    }
    let attribute_end = attributes
        .iter()
        .map(|attribute| {
            let relative = (attribute.offset as u32).saturating_sub(base);
            let element = attribute_element_size(attribute);
            relative.saturating_add(element)
        })
        .max()
        .unwrap_or(0);
    usize::try_from(
        base.checked_add(end.checked_sub(1)?.checked_mul(stride)?)?
            .checked_add(attribute_end)?,
    )
    .ok()
}

fn attribute_stride(attribute: &Attr) -> u32 {
    if attribute.stride > 0 {
        attribute.stride as u32
    } else {
        attribute_element_size(attribute)
    }
}

/// Whether this attribute's GL semantics cannot be handed to the IR as-is and must be converted to plain
/// `f32` components first.
///
/// The IR's vertex formats are WebGPU's, and three GL behaviours have no member there:
///
/// * `GL_FIXED` — 16.16 fixed point. There is no such vertex format, and the bytes were being declared
///   `f32` and reinterpreted, so every component decoded as a denormal ≈ 0.
/// * an integer component type with `normalized = GL_FALSE` feeding a `float`/`vec*` attribute. GL
///   converts the integer straight to float (40 → 40.0); WebGPU's `Uint8x4` and friends deliver an
///   *integer* to the shader, and a pipeline that feeds one to a float input is rejected outright — which
///   is what wedged the context, silently, for the rest of the process.
/// * a 1- or 3-component 8-/16-bit format, and a normalized 32-bit integer format. WebGPU has neither.
///
/// `glVertexAttribIPointer` attributes (`integer`) are excluded: those legitimately deliver integers, and
/// the 2-/4-component 32-bit forms they use are expressible.
///
/// That exclusion was unreachable for eight days and is the reason Chrome could not render a frame.
/// `glVertexAttribIPointer` recorded through the FLOAT entry point's recorder, which hard-coded
/// `integer = false`, so every integer array arrived here indistinguishable from
/// `glVertexAttribPointer(GL_UNSIGNED_INT, normalized = FALSE)` — which this function correctly converts.
/// The attribute was declared `Float32x2`, the shader's `uvec2` input required `Uint32x2`, and wgpu
/// refused the pipeline. The exclusion is real now because `record::vertex_attrib_ipointer` sets the flag;
/// the cross-check is `tests/lowering_extra/vformat.rs::an_ipointer_attribute_is_declared_integer_and_never_converted`,
/// which was watched failing before that flag existed.
fn needs_float_conversion(attribute: &Attr) -> bool {
    if attribute.integer {
        return false;
    }
    let comps = attribute.size.clamp(1, 4);
    match attribute.kind {
        GL_FIXED => true,
        GL_FLOAT => false,
        GL_HALF_FLOAT => matches!(comps, 1 | 3),
        GL_INT | GL_UNSIGNED_INT => true,
        GL_UNSIGNED_INT_2_10_10_10_REV => !attribute.normalized,
        // WebGPU exposes the unsigned normalized packed format only. Signed packed attributes must be
        // decoded to float explicitly even when GL normalization is enabled.
        GL_INT_2_10_10_10_REV => true,
        GL_BYTE | GL_UNSIGNED_BYTE | GL_SHORT | GL_UNSIGNED_SHORT => {
            !attribute.normalized || matches!(comps, 1 | 3)
        }
        _ => false,
    }
}

fn needs_float_width_conversion(program: &Program, location: usize, attribute: &Attr) -> bool {
    !attribute.integer
        && (attribute_stride(attribute) % 4 != 0
            || program
                .vertex_attr_components(location)
                .is_some_and(|components| components != attribute.size.clamp(1, 4)))
}

fn needs_integer_conversion(program: &Program, location: usize, attribute: &Attr) -> bool {
    if !attribute.integer {
        return false;
    }
    let components = attribute.size.clamp(1, 4);
    let narrow_unsupported = matches!(
        attribute.kind,
        GL_BYTE | GL_UNSIGNED_BYTE | GL_SHORT | GL_UNSIGNED_SHORT
    ) && matches!(components, 1 | 3);
    narrow_unsupported
        || attribute_stride(attribute) % 4 != 0
        || program
            .vertex_attr_components(location)
            .is_some_and(|linked| linked != components)
}

/// Decode one component of `attribute` from `bytes` at `offset` into the float GL says the shader sees.
///
/// Normalized conversion follows ES 3.0 §2.9.1: unsigned `c / (2^b − 1)`, signed `max(c / (2^(b−1) − 1),
/// −1)`. Unnormalized conversion is the plain numeric value. `GL_FIXED` is 16.16 fixed point, i.e. the
/// signed 32-bit value divided by 65536.
fn decode_component(attribute: &Attr, bytes: &[u8], offset: usize) -> f32 {
    let byte = |index: usize| bytes.get(offset + index).copied().unwrap_or(0);
    let two = || u16::from_le_bytes([byte(0), byte(1)]);
    let four = || u32::from_le_bytes([byte(0), byte(1), byte(2), byte(3)]);
    match attribute.kind {
        GL_FIXED => four() as i32 as f32 / 65536.0,
        GL_FLOAT => f32::from_bits(four()),
        GL_HALF_FLOAT => f32::from(half_to_f32(two())),
        GL_UNSIGNED_BYTE => {
            let value = f32::from(byte(0));
            if attribute.normalized {
                value / 255.0
            } else {
                value
            }
        }
        GL_BYTE => {
            let value = f32::from(byte(0) as i8);
            if attribute.normalized {
                (value / 127.0).max(-1.0)
            } else {
                value
            }
        }
        GL_UNSIGNED_SHORT => {
            let value = f32::from(two());
            if attribute.normalized {
                value / 65535.0
            } else {
                value
            }
        }
        GL_SHORT => {
            let value = f32::from(two() as i16);
            if attribute.normalized {
                (value / 32767.0).max(-1.0)
            } else {
                value
            }
        }
        GL_UNSIGNED_INT => {
            let value = four() as f32;
            if attribute.normalized {
                value / 4_294_967_295.0
            } else {
                value
            }
        }
        GL_INT => {
            let value = four() as i32 as f32;
            if attribute.normalized {
                (value / 2_147_483_647.0).max(-1.0)
            } else {
                value
            }
        }
        _ => 0.0,
    }
}

fn decode_attribute_component(attribute: &Attr, bytes: &[u8], base: usize, index: usize) -> f32 {
    if matches!(
        attribute.kind,
        GL_UNSIGNED_INT_2_10_10_10_REV | GL_INT_2_10_10_10_REV
    ) {
        let byte = |n: usize| bytes.get(base + n).copied().unwrap_or(0);
        let packed = u32::from_le_bytes([byte(0), byte(1), byte(2), byte(3)]);
        let bits = if index == 3 { 2 } else { 10 };
        let shift = if index == 3 { 30 } else { index * 10 };
        let mask = (1u32 << bits) - 1;
        let raw = (packed >> shift) & mask;
        if attribute.kind == GL_UNSIGNED_INT_2_10_10_10_REV {
            let value = raw as f32;
            if attribute.normalized {
                value / mask as f32
            } else {
                value
            }
        } else {
            let signed = ((raw << (32 - bits)) as i32) >> (32 - bits);
            let value = signed as f32;
            if attribute.normalized {
                (value / ((1u32 << (bits - 1)) - 1) as f32).max(-1.0)
            } else {
                value
            }
        }
    } else {
        let component = GlType(attribute.kind).component_size().max(1);
        decode_component(attribute, bytes, base + index * component)
    }
}

/// IEEE 754 half → single. Only reached by the 1-/3-component half formats WebGPU cannot express.
fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exponent = u32::from(bits >> 10) & 0x1f;
    let mantissa = u32::from(bits & 0x03ff);
    let bits = match exponent {
        0 if mantissa == 0 => sign,
        // Subnormal: renormalize into a single-precision exponent.
        0 => {
            let shift = mantissa.leading_zeros() - 21;
            sign | ((127 - 15 - shift) << 23) | ((mantissa << (shift + 1)) & 0x007f_ffff)
        }
        0x1f => sign | 0x7f80_0000 | (mantissa << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (mantissa << 13),
    };
    f32::from_bits(bits)
}

/// De-interleave one attribute out of its vertex buffer into a tightly-packed `f32` array, one element per
/// vertex the buffer can supply. Returns the bytes and the vertex count.
fn convert_attribute_to_f32_width(
    attribute: &Attr,
    source: &[u8],
    output_components: usize,
) -> (Vec<u8>, usize) {
    let source_components = attribute.size.clamp(1, 4) as usize;
    let stride = attribute_stride(attribute).max(1) as usize;
    let element = attribute_element_size(attribute) as usize;
    // Every vertex whose whole element lies inside the buffer: one at `offset`, then one per stride.
    let vertices = match source.len().checked_sub(attribute.offset + element) {
        Some(remaining) => 1 + remaining / stride,
        None => 0,
    };
    let mut out = Vec::with_capacity(vertices * output_components * size_of::<f32>());
    for vertex in 0..vertices {
        let base = attribute.offset + vertex * stride;
        for index in 0..output_components {
            let value = if index < source_components {
                decode_attribute_component(attribute, source, base, index)
            } else if index == 3 {
                1.0
            } else {
                0.0
            };
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    (out, vertices)
}

fn convert_attribute_to_integer_width(
    attribute: &Attr,
    source: &[u8],
    output_components: usize,
) -> (Vec<u8>, usize) {
    let source_components = attribute.size.clamp(1, 4) as usize;
    let stride = attribute_stride(attribute).max(1) as usize;
    let element = attribute_element_size(attribute) as usize;
    let component = GlType(attribute.kind).component_size().max(1);
    let vertices = match source.len().checked_sub(attribute.offset + element) {
        Some(remaining) => 1 + remaining / stride,
        None => 0,
    };
    let mut out = Vec::with_capacity(vertices * output_components * 4);
    for vertex in 0..vertices {
        let base = attribute.offset + vertex * stride;
        for index in 0..output_components {
            let value = if index >= source_components {
                u32::from(index == 3)
            } else {
                let offset = base + index * component;
                let byte = |n| source.get(offset + n).copied().unwrap_or(0);
                match attribute.kind {
                    GL_BYTE => byte(0) as i8 as i32 as u32,
                    GL_UNSIGNED_BYTE => byte(0) as u32,
                    GL_SHORT => i16::from_le_bytes([byte(0), byte(1)]) as i32 as u32,
                    GL_UNSIGNED_SHORT => u16::from_le_bytes([byte(0), byte(1)]) as u32,
                    GL_INT | GL_UNSIGNED_INT => {
                        u32::from_le_bytes([byte(0), byte(1), byte(2), byte(3)])
                    }
                    _ => 0,
                }
            };
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    (out, vertices)
}

fn attribute_element_size(attribute: &Attr) -> u32 {
    GlType(attribute.kind).vertex_element_size(attribute.size) as u32
}
pub(super) fn lower_vertices(
    ctx: &mut GlContext,
    program: &Program,
    d: &DrawCall,
    cmds: &mut Vec<Cmd>,
) -> hl_gpu::Result<VertexLowering> {
    let captured_buffer = |name: u32| d.buffers.iter().find(|buffer| buffer.name == name);
    let has_data = |name: u32| {
        captured_buffer(name)
            .map(|buffer| !buffer.data.is_empty())
            .unwrap_or_else(|| ctx.buffers.has_data(name))
    };
    // ---- vertex-buffer slot analysis (dedup bound buffers into slots) ----
    let mut slot_gl_buf: Vec<u32> = Vec::new();
    let mut attr_slot = [-1i32; crate::model::program::MAX_ATTR];
    for (i, a) in d.attrs.iter().enumerate() {
        if !a.enabled || a.buffer == 0 || !has_data(a.buffer) {
            continue;
        }
        let sl = slot_gl_buf
            .iter()
            .position(|&x| x == a.buffer)
            .unwrap_or_else(|| {
                slot_gl_buf.push(a.buffer);
                slot_gl_buf.len() - 1
            });
        attr_slot[i] = sl as i32;
    }
    let nslot = slot_gl_buf.len();
    if d.attrs
        .iter()
        .any(|attr| attr.enabled && !has_data(attr.buffer))
    {
        let attributes = d
            .attrs
            .iter()
            .enumerate()
            .filter(|(_, attr)| attr.enabled)
            .map(|(location, attr)| {
                (
                    location,
                    attr.buffer,
                    attr.binding,
                    attr.offset,
                    attr.stride,
                    ctx.buffers
                        .get(attr.buffer)
                        .map(|buffer| buffer.data.len())
                        .unwrap_or(0),
                )
            })
            .collect::<Vec<_>>();
        hl_log::hl_warn!(
            hl_log::tag::GL,
            "enabled vertex attributes include unpopulated buffers: {attributes:?}"
        );
    }
    let mut slot_stride = vec![0u32; nslot.max(1)];
    for (i, a) in d.attrs.iter().enumerate() {
        let sl = attr_slot[i];
        if sl < 0 {
            continue;
        }
        let st = attribute_stride(a);
        if st > slot_stride[sl as usize] {
            slot_stride[sl as usize] = st;
        }
    }
    for st in slot_stride.iter_mut() {
        if *st == 0 {
            *st = 16;
        }
    }
    // Per-slot BASE byte offset — the whole-stride multiple hoisted out of the attributes into the
    // vertex-buffer BIND offset (`Enc::SetVertexBuffer { offset }`). GskGpu's vertex-pulling model
    // (position from `gl_VertexID`, real data PER-INSTANCE) draws every op's instances out of ONE big
    // frame VBO; with the `base-instance` GL feature UNAVAILABLE it bakes the per-instance region base
    // (`first_instance * stride`) straight into the `glVertexAttribPointer` offset, so an attribute's GL
    // offset routinely runs into the tens of thousands (e.g. instance 542 → offset 26016). wgpu forbids a
    // vertex attribute whose `offset + format_size` exceeds the buffer's `array_stride`, so the
    // stride-multiple part of the offset is moved to the buffer bind offset, leaving each attribute's
    // in-stride offset in `[0, stride)`. For an ordinary draw every attribute offset is already `< stride`,
    // so the base is `0` and the lowering is byte-identical to before.
    let mut slot_base = vec![0u32; nslot.max(1)];
    for sl in 0..nslot {
        let min_off = d
            .attrs
            .iter()
            .enumerate()
            .filter(|(i, a)| attr_slot[*i] == sl as i32 && a.enabled)
            .map(|(_, a)| a.offset as u32)
            .min();
        if let Some(min_off) = min_off {
            let stride = slot_stride[sl].max(1);
            slot_base[sl] = (min_off / stride) * stride;
        }
    }
    // GL gives every attribute its OWN stride and offset into the bound buffer, so one VBO may hold several
    // separate, non-interleaved attribute regions (glmark2's jellyfish and ideas scenes upload positions,
    // normals and texcoords as consecutive tight arrays in one buffer). Folding those into a single slot with
    // one stride misdescribes them: the attribute's offset then exceeds the slot's array stride and the host
    // rejects the pipeline ("Vertex attribute at location 1 stride 40 exceeds the limit 8"). An attribute that
    // does not fit its buffer's shared (stride, base) therefore gets its own vertex-buffer slot over the SAME
    // buffer, bound at its own byte offset — which the neutral IR expresses directly, since `VertexLayout`
    // carries a per-slot stride and `Enc::SetVertexBuffer` a per-slot bind offset. Attributes that already fit
    // (every interleaved layout, and GskGpu's hoisted per-instance base) keep their shared slot unchanged.
    let mut nslot = nslot;
    for (location, attribute) in d.attrs.iter().enumerate() {
        let slot = attr_slot[location];
        if slot < 0 || !attribute.enabled {
            continue;
        }
        let slot = slot as usize;
        let element = attribute_element_size(attribute);
        let relative = (attribute.offset as u32).saturating_sub(slot_base[slot]);
        if relative.saturating_add(element) <= slot_stride[slot] {
            continue;
        }
        let stride = attribute_stride(attribute);
        // Prefer a 4-aligned bind offset (WebGPU requires one); fall back to the exact attribute offset when
        // the aligned remainder would not fit inside one stride.
        let offset = attribute.offset as u32;
        let aligned = offset & !3;
        let base = if (offset - aligned).saturating_add(element) <= stride {
            aligned
        } else {
            offset
        };
        let step = (attribute.divisor > 0) as u32;
        let existing = (0..nslot).position(|candidate| {
            slot_gl_buf[candidate] == attribute.buffer
                && slot_stride[candidate] == stride
                && slot_base[candidate] == base
                && (0..d.attrs.len())
                    .any(|other| attr_slot[other] == candidate as i32 && d.attrs[other].divisor > 0)
                    == (step == 1)
        });
        attr_slot[location] = match existing {
            Some(candidate) => candidate as i32,
            None => {
                slot_gl_buf.push(attribute.buffer);
                slot_stride.push(stride);
                slot_base.push(base);
                nslot += 1;
                (nslot - 1) as i32
            }
        };
    }
    let nvd = d
        .attrs
        .iter()
        .enumerate()
        .filter(|(_, a)| a.enabled)
        .map(|(i, _)| i + 1)
        .max()
        .unwrap_or(0);

    // Resolve the IR buffer id for each vertex slot, uploading its bytes ONLY on first sight / content
    // change (the residency cache): a VBO bound across many draws or frames is created + written once.
    let mut slot_ir: Vec<u32> = Vec::with_capacity(nslot);
    let mut slot_bytes: Vec<usize> = Vec::with_capacity(nslot);
    for (slot, &gl_buf) in slot_gl_buf.iter().enumerate() {
        let captured = captured_buffer(gl_buf);
        let generation = captured
            .map(|buffer| buffer.generation)
            .or_else(|| ctx.buffers.get(gl_buf).map(|buffer| buffer.gen))
            .unwrap_or(0);
        let mut data = captured_buffer(gl_buf)
            .map(|buffer| buffer.data.clone())
            .or_else(|| ctx.buffers.get(gl_buf).map(|buffer| buffer.data.clone()))
            .unwrap_or_default();
        let attributes = d
            .attrs
            .iter()
            .enumerate()
            .filter(|(location, attribute)| {
                attribute.enabled && attr_slot[*location] == slot as i32
            })
            .map(|(_, attribute)| *attribute)
            .collect::<Vec<_>>();
        let required =
            vertex_slot_bytes(d, &attributes, slot_stride[slot], slot_base[slot]).unwrap_or(0);
        if required > data.len() {
            let shape = attributes
                .iter()
                .map(|attribute| {
                    (
                        attribute.offset,
                        attribute.stride,
                        attribute.size,
                        attribute.kind,
                        attribute.divisor,
                    )
                })
                .collect::<Vec<_>>();
            // A robust GL context permits an out-of-range vertex fetch and supplies zero for inaccessible
            // components. WebGPU rejects the entire command buffer before execution instead. Preserve the
            // GL behavior with a draw-local padded snapshot; valid buffers keep using the residency cache.
            hl_log::hl_warn!(
                hl_log::tag::GL,
                "padding undersized lowered vertex slot kind=vbo slot={slot} gl_buffer={gl_buf} \
                 bytes={} required={required} base={} stride={} step_mode={} first={} count={} attrs={shape:?}",
                data.len(),
                slot_base[slot],
                slot_stride[slot],
                attributes.iter().any(|attribute| attribute.divisor > 0) as u32,
                d.first,
                d.count
            );
            std::sync::Arc::make_mut(&mut data).resize(required, 0);
            slot_bytes.push(data.len());
            let ir = ctx.alloc_buffer_ir()?;
            slot_ir.push(ir);
            cmds.push(Cmd::CreateBuffer(
                ir,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: format!("gl-padded-vertex:{gl_buf}:{generation}"),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id: ir,
                offset: 0,
                data: data.as_ref().clone(),
            });
            continue;
        }
        slot_bytes.push(data.len());
        let (ir, needs_upload) = ctx.data_buffer_ir(gl_buf, buffer_usage::VERTEX, generation)?;
        slot_ir.push(ir);
        if needs_upload {
            cmds.push(Cmd::CreateBuffer(
                ir,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: format!("gl-bound-vertex:{gl_buf}:{generation}"),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id: ir,
                offset: 0,
                data: data.as_ref().clone(),
            });
        }
    }

    // ---- client-side vertex arrays (no VBO bound) → transient per-draw VERTEX buffers ----
    // Each captured client array (recorded at draw time from a `glVertexAttribPointer` client pointer)
    // becomes its own tightly-packed buffer + a one-attribute vertex-layout slot appended AFTER the VBO
    // slots. De-interleaving into per-attribute buffers maps 1:1 onto the vertex-layout IR and handles
    // interleaved and separate client arrays uniformly. EMPTY for a bound-VBO draw → that path is unchanged.
    let mut client_slots: Vec<ClientSlot> = Vec::with_capacity(d.client_vbufs.len());

    // ---- attribute formats the IR cannot express → a converted, tightly-packed f32 slot ----
    // `GL_FIXED`, an unnormalized integer type feeding a float attribute, and the 1-/3-component 8-/16-bit
    // forms have no WebGPU vertex format (see [`needs_float_conversion`]). Each is de-interleaved out of
    // its vertex buffer and converted here, then appended as its own single-attribute slot — reusing the
    // client-array machinery, so the location is excluded from its VBO slot's layout and nothing else in
    // the lowering changes. Handing these to the IR raw declared an INTEGER format for a float shader
    // input, which wgpu rejects outright and which wedged the context for the rest of the process.
    for (location, attribute) in d.attrs.iter().enumerate() {
        if !attribute.enabled
            || attr_slot[location] < 0
            || !(needs_float_conversion(attribute)
                || needs_float_width_conversion(program, location, attribute)
                || needs_integer_conversion(program, location, attribute))
        {
            continue;
        }
        let source = captured_buffer(attribute.buffer)
            .map(|buffer| buffer.data.clone())
            .or_else(|| ctx.buffers.get(attribute.buffer).map(|b| b.data.clone()))
            .unwrap_or_default();
        let components = program
            .vertex_attr_components(location)
            .unwrap_or(attribute.size)
            .clamp(1, 4);
        let integer = needs_integer_conversion(program, location, attribute);
        let (data, vertices) = if integer {
            convert_attribute_to_integer_width(attribute, &source, components as usize)
        } else {
            convert_attribute_to_f32_width(attribute, &source, components as usize)
        };
        if vertices == 0 {
            continue;
        }
        let comps = components as u32;
        let ir = ctx.alloc_buffer_ir()?;
        cmds.push(Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::VERTEX,
                label: format!("gl-converted-vertex:{:#x}", attribute.kind),
            },
        ));
        let bytes = data.len();
        cmds.push(Cmd::WriteBuffer {
            id: ir,
            offset: 0,
            data,
        });
        client_slots.push(ClientSlot {
            ir,
            bytes,
            stride: comps * size_of::<f32>() as u32,
            step_mode: (attribute.divisor > 0) as u32,
            location: location as u32,
            format: vertex_format_wire(
                if integer {
                    if matches!(attribute.kind, GL_BYTE | GL_SHORT | GL_INT) {
                        GL_INT
                    } else {
                        GL_UNSIGNED_INT
                    }
                } else {
                    GL_FLOAT
                },
                comps as i32,
                false,
                integer,
            ),
        });
    }

    for ca in &d.client_vbufs {
        let mut attribute = d.attrs[ca.location];
        attribute.offset = 0;
        attribute.stride = 0;
        let linked_components = program
            .vertex_attr_components(ca.location)
            .unwrap_or(ca.size)
            .clamp(1, 4);
        let float_layout_conversion =
            needs_float_width_conversion(program, ca.location, &attribute);
        let integer_conversion = needs_integer_conversion(program, ca.location, &attribute);
        let (data, kind, normalized, integer, components) = if needs_float_conversion(&attribute)
            || float_layout_conversion
        {
            let (converted, _) =
                convert_attribute_to_f32_width(&attribute, &ca.data, linked_components as usize);
            (converted, GL_FLOAT, false, false, linked_components)
        } else if integer_conversion {
            let (converted, _) = convert_attribute_to_integer_width(
                &attribute,
                &ca.data,
                linked_components as usize,
            );
            let kind = if matches!(attribute.kind, GL_BYTE | GL_SHORT | GL_INT) {
                GL_INT
            } else {
                GL_UNSIGNED_INT
            };
            (converted, kind, false, true, linked_components)
        } else {
            (ca.data.clone(), ca.kind, ca.normalized, ca.integer, ca.size)
        };
        let bytes = data.len();
        let ir = ctx.alloc_buffer_ir()?;
        cmds.push(Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::VERTEX,
                label: if kind == GL_FLOAT && ca.kind != GL_FLOAT {
                    "gl-converted-client-vertex".to_owned()
                } else {
                    "gl-client-vertex".to_owned()
                },
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: ir,
            offset: 0,
            data,
        });
        let elem = GlType(kind).vertex_element_size(components) as u32;
        client_slots.push(ClientSlot {
            ir,
            bytes,
            stride: elem.max(1),
            step_mode: (ca.divisor > 0) as u32,
            location: ca.location as u32,
            format: vertex_format_wire(kind, components, normalized, integer),
        });
    }
    // A disabled GL attribute array reads the context's current generic attribute value. WebGPU has no
    // constant-attribute state, so materialize that value as a tiny instance-stepped buffer. Repeat it for
    // every addressed instance so indexed/non-indexed vertex counts do not inflate the upload.
    let constant_components = (0..d.attrs.len())
        .map(|location| program.vertex_attr_components(location))
        .collect::<Vec<_>>();
    for (location, components) in constant_components.iter().copied().enumerate() {
        if d.attrs[location].enabled {
            continue;
        }
        let Some(size) = components else {
            continue;
        };
        let repeats = d
            .first_instance
            .saturating_add(d.instance_count.max(1))
            .max(1) as usize;
        let components = size as usize;
        let mut data = Vec::with_capacity(repeats * components * size_of::<f32>());
        for _ in 0..repeats {
            data.extend(
                d.current_attrs[location][..components]
                    .iter()
                    .flat_map(|component| component.to_bits().to_le_bytes()),
            );
        }
        // Kind and integer-ness are decided together, from one discriminant. They were two expressions
        // of one fact — the kind here and `kind != GL_FLOAT` at the format below — which is the third
        // source of truth for "is this attribute an integer" and the shape that let the
        // `glVertexAttribIPointer` defect survive since July. One match, one answer.
        let (kind, integer) = match d.current_attr_kinds[location] {
            1 => (GL_INT, true),
            2 => (GL_UNSIGNED_INT, true),
            _ => (GL_FLOAT, false),
        };
        let ir = ctx.alloc_buffer_ir()?;
        cmds.push(Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::VERTEX,
                label: "gl-constant-vertex".to_owned(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: ir,
            offset: 0,
            data,
        });
        client_slots.push(ClientSlot {
            ir,
            bytes: repeats * components * size_of::<f32>(),
            stride: size as u32 * size_of::<f32>() as u32,
            step_mode: 1,
            location: location as u32,
            format: vertex_format_wire(kind, size, false, integer),
        });
    }

    if !d.indexed {
        let vertex_end = d.first.max(0).saturating_add(d.count.max(0)) as usize;
        for (slot, client) in client_slots.iter().enumerate() {
            if client.step_mode == 0 && client.bytes / (client.stride.max(1) as usize) < vertex_end
            {
                hl_log::hl_warn!(
                    hl_log::tag::GL,
                    "undersized lowered vertex slot kind=client slot={} ir={} bytes={} base=0 stride={} \
                     step_mode={} location={} first={} count={} vertex_end={}",
                    nslot + slot,
                    client.ir,
                    client.bytes,
                    client.stride,
                    client.step_mode,
                    client.location,
                    d.first,
                    d.count,
                    vertex_end
                );
            }
        }
    }

    // Primitive forms absent from the neutral topology enum are expanded into exact indexed lists.
    // WebGPU also has no u8 index format, so a bound GL_UNSIGNED_BYTE EBO is promoted here rather than
    // being misread as pairs of u16 bytes.
    let expand_primitive = matches!(d.mode, 0x0002 | 0x0006);
    let promote_u8 = d.indexed && d.elem_buf != 0 && d.index_type == GL_UNSIGNED_BYTE;
    let expanded_indices = if expand_primitive || promote_u8 {
        if d.indexed {
            let source = if d.elem_buf != 0 {
                captured_buffer(d.elem_buf)
                    .map(|buffer| buffer.data.as_slice())
                    .or_else(|| {
                        ctx.buffers
                            .get(d.elem_buf)
                            .map(|buffer| buffer.data.as_slice())
                    })
            } else if d.client_indices.is_empty() {
                None
            } else {
                Some(d.client_indices.as_slice())
            };
            let offset = if d.elem_buf != 0 { d.index_offset } else { 0 };
            source
                .and_then(|bytes| {
                    PrimitiveAssembly::decode_indices(bytes, offset, d.index_type, d.count)
                })
                .map(|indices| {
                    if expand_primitive {
                        PrimitiveAssembly::expand(d.mode, &indices)
                    } else {
                        indices
                    }
                })
        } else {
            PrimitiveAssembly::expanded_array_indices(d.mode, d.first, d.count)
        }
    } else {
        None
    };

    // Index buffer: an exact primitive expansion, a bound element-array-buffer, or the captured
    // client-side index array (transient).
    let mut index_ir = 0u32;
    if let Some(indices) = expanded_indices
        .as_ref()
        .filter(|indices| !indices.is_empty())
    {
        index_ir = ctx.alloc_buffer_ir()?;
        let data = indices
            .iter()
            .flat_map(|index| index.to_le_bytes())
            .collect::<Vec<_>>();
        cmds.push(Cmd::CreateBuffer(
            index_ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::INDEX,
                label: "gl-expanded-index".to_owned(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: index_ir,
            offset: 0,
            data,
        });
    } else if d.indexed
        && d.elem_buf != 0
        && (captured_buffer(d.elem_buf).is_some_and(|buffer| !buffer.data.is_empty())
            || ctx.buffers.has_data(d.elem_buf))
    {
        let captured = captured_buffer(d.elem_buf);
        let generation = captured
            .map(|buffer| buffer.generation)
            .or_else(|| ctx.buffers.get(d.elem_buf).map(|buffer| buffer.gen))
            .unwrap_or(0);
        let (ir, needs_upload) = ctx.data_buffer_ir(d.elem_buf, buffer_usage::INDEX, generation)?;
        index_ir = ir;
        if needs_upload {
            let data = captured_buffer(d.elem_buf)
                .map(|buffer| buffer.data.clone())
                .or_else(|| {
                    ctx.buffers
                        .get(d.elem_buf)
                        .map(|buffer| buffer.data.clone())
                })
                .unwrap_or_default();
            cmds.push(Cmd::CreateBuffer(
                index_ir,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::INDEX,
                    label: format!("gl-bound-index:{}:{generation}", d.elem_buf),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id: index_ir,
                offset: 0,
                data: data.as_ref().clone(),
            });
        }
    } else if d.indexed && !d.client_indices.is_empty() {
        index_ir = ctx.alloc_buffer_ir()?;
        let data = d.client_indices.clone();
        cmds.push(Cmd::CreateBuffer(
            index_ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::INDEX,
                label: "gl-client-index".to_owned(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: index_ir,
            offset: 0,
            data,
        });
    }

    // Conversion appends a replacement slot. Remove a source slot when every attribute it fed was
    // converted: WebGPU validates even an empty layout's stride, and backends need no placeholder between
    // the remaining direct layouts and their actual buffer bindings.
    let mut old_to_new = vec![None; nslot];
    let mut next_slot = 0usize;
    for (slot, mapped) in old_to_new.iter_mut().enumerate() {
        let feeds_direct_attribute = d.attrs.iter().enumerate().any(|(location, attribute)| {
            attribute.enabled
                && attr_slot[location] == slot as i32
                && !(needs_float_conversion(attribute)
                    || needs_float_width_conversion(program, location, attribute)
                    || needs_integer_conversion(program, location, attribute))
        });
        if feeds_direct_attribute {
            *mapped = Some(next_slot);
            next_slot += 1;
        }
    }
    for (location, slot) in attr_slot.iter_mut().enumerate() {
        if *slot < 0 {
            continue;
        }
        *slot = if needs_float_conversion(&d.attrs[location])
            || needs_float_width_conversion(program, location, &d.attrs[location])
            || needs_integer_conversion(program, location, &d.attrs[location])
        {
            -1
        } else {
            old_to_new[*slot as usize].map_or(-1, |mapped| mapped as i32)
        };
    }
    slot_stride.truncate(nslot);
    slot_base.truncate(nslot);
    slot_stride = slot_stride
        .into_iter()
        .enumerate()
        .filter_map(|(slot, value)| old_to_new[slot].map(|_| value))
        .collect();
    slot_base = slot_base
        .into_iter()
        .enumerate()
        .filter_map(|(slot, value)| old_to_new[slot].map(|_| value))
        .collect();
    slot_ir = slot_ir
        .into_iter()
        .enumerate()
        .filter_map(|(slot, value)| old_to_new[slot].map(|_| value))
        .collect();
    slot_bytes = slot_bytes
        .into_iter()
        .enumerate()
        .filter_map(|(slot, value)| old_to_new[slot].map(|_| value))
        .collect();
    nslot = next_slot;

    Ok(VertexLowering {
        nslot,
        slot_stride,
        slot_base,
        slot_ir,
        slot_bytes,
        attr_slot,
        nvd,
        client_slots,
        expanded_indices,
        index_ir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertex_range_includes_nonzero_first_vertex() {
        let draw = DrawCall {
            first: 50,
            count: 40,
            ..DrawCall::default()
        };
        let attribute = Attr {
            enabled: true,
            size: 2,
            kind: GL_FLOAT,
            stride: 16,
            ..Attr::default()
        };

        assert_eq!(vertex_slot_bytes(&draw, &[attribute], 16, 0), Some(1432));
    }

    #[test]
    fn vertex_range_includes_hoisted_binding_base() {
        let draw = DrawCall {
            first: 50,
            count: 40,
            ..DrawCall::default()
        };
        let attribute = Attr {
            enabled: true,
            size: 2,
            kind: GL_FLOAT,
            stride: 16,
            offset: 4_808,
            ..Attr::default()
        };

        assert_eq!(
            vertex_slot_bytes(&draw, &[attribute], 16, 4_800),
            Some(6_240)
        );
    }

    #[test]
    fn overflowing_vertex_range_is_not_sized_for_padding() {
        let draw = DrawCall {
            first: i32::MAX - 1,
            count: i32::MAX,
            ..DrawCall::default()
        };
        let attribute = Attr {
            enabled: true,
            size: 2,
            kind: GL_FLOAT,
            stride: 16,
            ..Attr::default()
        };

        assert_eq!(vertex_slot_bytes(&draw, &[attribute], 16, 0), None);
    }

    #[test]
    fn tightly_packed_unsigned_byte_attribute_uses_byte_stride() {
        let attribute = Attr {
            size: 4,
            kind: GL_UNSIGNED_BYTE,
            stride: 0,
            ..Attr::default()
        };

        assert_eq!(attribute_stride(&attribute), 4);
    }

    #[test]
    fn tightly_packed_unsigned_short_attribute_uses_short_stride() {
        let attribute = Attr {
            size: 2,
            kind: GL_UNSIGNED_SHORT,
            stride: 0,
            ..Attr::default()
        };

        assert_eq!(attribute_stride(&attribute), 4);
    }

    #[test]
    fn packed_2_10_10_10_attribute_is_one_four_byte_element() {
        let attribute = Attr {
            size: 4,
            kind: GL_UNSIGNED_INT_2_10_10_10_REV,
            normalized: true,
            stride: 0,
            ..Attr::default()
        };

        assert_eq!(attribute_stride(&attribute), 4);
        assert_eq!(
            vertex_format_wire(attribute.kind, 4, true, false),
            4 | (8 << 8) | (1 << 16)
        );
    }

    #[test]
    fn signed_packed_attribute_converts_all_four_normalized_components() {
        let attribute = Attr {
            size: 4,
            kind: GL_INT_2_10_10_10_REV,
            normalized: true,
            ..Attr::default()
        };
        // x=-512, y=511, z=-1, w=-2 in REV field order.
        let packed = 0x200 | (0x1ff << 10) | (0x3ff << 20) | (0x2 << 30);
        let (bytes, vertices) =
            convert_attribute_to_f32_width(&attribute, &u32::to_le_bytes(packed), 4);
        let values = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect::<Vec<_>>();

        assert_eq!(vertices, 1);
        assert_eq!(values, vec![-1.0, 1.0, -1.0 / 511.0, -1.0]);
    }
}
