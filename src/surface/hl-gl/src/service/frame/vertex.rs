use super::*;

pub(super) struct ClientSlot {
    pub(super) ir: u32,
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
    pub(super) attr_slot: [i32; crate::model::program::MAX_ATTR],
    pub(super) nvd: usize,
    pub(super) client_slots: Vec<ClientSlot>,
    pub(super) expanded_indices: Option<Vec<u32>>,
    pub(super) index_ir: u32,
}

pub(super) fn lower_vertices(
    ctx: &mut GlContext,
    d: &DrawCall,
    cmds: &mut Vec<Cmd>,
) -> VertexLowering {
    let captured_buffer = |name: u32| d.buffers.iter().find(|buffer| buffer.name == name);
    // ---- vertex-buffer slot analysis (dedup bound buffers into slots) ----
    let mut slot_gl_buf: Vec<u32> = Vec::new();
    let mut attr_slot = [-1i32; crate::model::program::MAX_ATTR];
    for (i, a) in d.attrs.iter().enumerate() {
        if !a.enabled || a.buffer == 0 || !ctx.buffers.has_data(a.buffer) {
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
    let mut slot_stride = vec![0u32; nslot.max(1)];
    for (i, a) in d.attrs.iter().enumerate() {
        let sl = attr_slot[i];
        if sl < 0 {
            continue;
        }
        let mut st = a.stride as u32;
        if st == 0 {
            st = a.size as u32 * 4;
        }
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
    for &gl_buf in &slot_gl_buf {
        let captured = captured_buffer(gl_buf);
        let generation = captured
            .map(|buffer| buffer.generation)
            .or_else(|| ctx.buffers.get(gl_buf).map(|buffer| buffer.gen))
            .unwrap_or(0);
        let (ir, needs_upload) = ctx.data_buffer_ir(gl_buf, buffer_usage::VERTEX, generation);
        slot_ir.push(ir);
        if needs_upload {
            let data = captured_buffer(gl_buf)
                .map(|buffer| buffer.data.clone())
                .or_else(|| ctx.buffers.get(gl_buf).map(|buffer| buffer.data.clone()))
                .unwrap_or_default();
            cmds.push(Cmd::CreateBuffer(
                ir,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id: ir,
                offset: 0,
                data,
            });
        }
    }

    // ---- client-side vertex arrays (no VBO bound) → transient per-draw VERTEX buffers ----
    // Each captured client array (recorded at draw time from a `glVertexAttribPointer` client pointer)
    // becomes its own tightly-packed buffer + a one-attribute vertex-layout slot appended AFTER the VBO
    // slots. De-interleaving into per-attribute buffers maps 1:1 onto the vertex-layout IR and handles
    // interleaved and separate client arrays uniformly. EMPTY for a bound-VBO draw → that path is unchanged.
    let mut client_slots: Vec<ClientSlot> = Vec::with_capacity(d.client_vbufs.len());
    for ca in &d.client_vbufs {
        let ir = ctx.alloc_buffer_ir();
        cmds.push(Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size: ca.data.len() as u64,
                usage: buffer_usage::VERTEX,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: ir,
            offset: 0,
            data: ca.data.clone(),
        });
        let elem = ca.size.clamp(1, 4) as u32 * GlType(ca.kind).component_size() as u32;
        client_slots.push(ClientSlot {
            ir,
            stride: elem.max(1),
            step_mode: (ca.divisor > 0) as u32,
            location: ca.location as u32,
            format: vertex_format_wire(ca.kind, ca.size, ca.normalized, ca.integer),
        });
    }

    // Primitive forms absent from the neutral topology enum are expanded into exact indexed lists.
    let expanded_indices = if matches!(d.mode, 0x0002 | 0x0006) {
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
                .map(|indices| PrimitiveAssembly::expand(d.mode, &indices))
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
        index_ir = ctx.alloc_buffer_ir();
        let data = indices
            .iter()
            .flat_map(|index| index.to_le_bytes())
            .collect::<Vec<_>>();
        cmds.push(Cmd::CreateBuffer(
            index_ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::INDEX,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: index_ir,
            offset: 0,
            data,
        });
    } else if d.indexed && d.elem_buf != 0 && ctx.buffers.has_data(d.elem_buf) {
        let captured = captured_buffer(d.elem_buf);
        let generation = captured
            .map(|buffer| buffer.generation)
            .or_else(|| ctx.buffers.get(d.elem_buf).map(|buffer| buffer.gen))
            .unwrap_or(0);
        let (ir, needs_upload) = ctx.data_buffer_ir(d.elem_buf, buffer_usage::INDEX, generation);
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
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::WriteBuffer {
                id: index_ir,
                offset: 0,
                data,
            });
        }
    } else if d.indexed && !d.client_indices.is_empty() {
        index_ir = ctx.alloc_buffer_ir();
        let data = d.client_indices.clone();
        cmds.push(Cmd::CreateBuffer(
            index_ir,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::INDEX,
                label: String::new(),
            },
        ));
        cmds.push(Cmd::WriteBuffer {
            id: index_ir,
            offset: 0,
            data,
        });
    }

    VertexLowering {
        nslot,
        slot_stride,
        slot_base,
        slot_ir,
        attr_slot,
        nvd,
        client_slots,
        expanded_indices,
        index_ir,
    }
}
