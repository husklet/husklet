use super::*;

// ---------------------------------------------------------------------------------------------------
// Vertex attribute formats the IR cannot express are CONVERTED to f32, not handed over raw
//
// Three GL behaviours have no WebGPU vertex format: GL_FIXED (16.16), an unnormalized integer type
// feeding a float attribute, and the 1-/3-component 8-/16-bit forms. Handing them over raw declared an
// INTEGER format for a float shader input, which wgpu rejects — wedging the context for the rest of the
// process — or, for GL_FIXED, reinterpreted the fixed-point bits as f32 and produced denormals ≈ 0.
// ---------------------------------------------------------------------------------------------------

const TINT_VS: &str = "attribute vec2 aPos;\nattribute vec4 tint;\nvarying vec4 v;\n\
                       void main(){ v = tint; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const TINT_FS: &str = "varying vec4 v;\nvoid main(){ gl_FragColor = v; }\n";

/// Link the two-attribute program and bind a position VBO on location 0.
fn setup_tint(c: &mut GlContext) {
    let v = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, v, TINT_VS);
    record::compile_shader(c, v);
    let f = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, f, TINT_FS);
    record::compile_shader(c, f);
    let p = record::create_program(c);
    record::attach_shader(c, p, v);
    record::attach_shader(c, p, f);
    assert!(record::link_program(c, p));
    record::use_program(c, p);
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);
    record::viewport(c, [0, 0, 256, 256]);
}

/// Bind `bytes` as location 1's array with the given format, lower the draw, and return the f32
/// components the converted slot uploaded (empty when nothing was converted).
fn converted_components(size: i32, kind: u32, normalized: bool, bytes: &[u8]) -> Vec<f32> {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let tint = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, tint);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, bytes, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 1, size, kind, normalized, 0, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    sink.batches[0]
        .iter()
        .find_map(|cmd| match cmd {
            Cmd::CreateBuffer(_, desc) if desc.label.starts_with("gl-converted-vertex") => {
                Some(desc.size)
            }
            _ => None,
        })
        .expect("a converted vertex buffer");
    let data = sink.batches[0]
        .iter()
        .zip(sink.batches[0].iter().skip(1))
        .find_map(|(create, write)| match (create, write) {
            (Cmd::CreateBuffer(_, desc), Cmd::WriteBuffer { data, .. })
                if desc.label.starts_with("gl-converted-vertex") =>
            {
                Some(data.clone())
            }
            _ => None,
        })
        .expect("the converted bytes");
    data.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[test]
fn gl_fixed_decodes_as_16_16_fixed_point() {
    // 16.16: the stored integer divided by 65536. 0.25 → 16384, 1.0 → 65536, -2.5 → -163840.
    let values: [i32; 4] = [16384, 65536, -163840, 0];
    let bytes = values
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let out = converted_components(4, GL_FIXED, false, &bytes);
    assert_eq!(&out[..4], &[0.25, 1.0, -2.5, 0.0]);
}

#[test]
fn an_unnormalized_integer_attribute_becomes_its_plain_numeric_value() {
    // GL: normalized = FALSE means the integer converts straight to float, so 40 → 40.0 (NOT 40/255).
    let out = converted_components(4, GL_UNSIGNED_BYTE, false, &[40, 80, 120, 160]);
    assert_eq!(&out[..4], &[40.0, 80.0, 120.0, 160.0]);

    let shorts: [u16; 4] = [40, 80, 120, 160];
    let bytes = shorts
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let out = converted_components(4, GL_UNSIGNED_SHORT, false, &bytes);
    assert_eq!(&out[..4], &[40.0, 80.0, 120.0, 160.0]);
}

#[test]
fn a_three_component_normalized_byte_attribute_is_converted_not_refused() {
    // WebGPU has no Unorm8x3, so this reached the pipeline as an error and wedged the context.
    // 255/255 = 1.0, 128/255 = 0.50196…, 0/255 = 0.
    let out = converted_components(3, GL_UNSIGNED_BYTE, true, &[255, 128, 0, 0]);
    assert_eq!(out[0], 1.0);
    assert!((out[1] - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(out[2], 0.0);
}

#[test]
fn bound_float_attribute_is_padded_to_the_linked_shader_width() {
    let values = [0.25f32, 0.5, 0.75, 1.0, -0.25, -0.5];
    let bytes = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let out = converted_components(2, GL_FLOAT, false, &bytes);
    assert_eq!(&out[..4], &[0.25, 0.5, 0.0, 1.0]);
}

#[test]
fn unaligned_bound_stride_is_repacked_to_a_webgpu_aligned_slot() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let tint = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, tint);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 51], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 1, 4, GL_FLOAT, false, 17, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let attr = pipeline_desc(&sink.batches[0])
        .vertex_buffers
        .iter()
        .find(|layout| layout.attrs.iter().any(|attr| attr.location == 1))
        .expect("repacked attribute");
    assert_eq!(attr.stride, 16);
}

#[test]
fn converted_tightly_packed_vec3_leaves_no_empty_vertex_layout() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let tint = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, tint);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[255, 128, 0, 64, 32, 16, 8, 4, 2], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 1, 3, GL_UNSIGNED_BYTE, true, 0, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());

    let pipeline = pipeline_desc(&sink.batches[0]);
    assert!(
        pipeline
            .vertex_buffers
            .iter()
            .all(|layout| !layout.attrs.is_empty()),
        "the converted replacement slot makes its source layout unnecessary"
    );
    let converted = pipeline
        .vertex_buffers
        .iter()
        .find(|layout| layout.attrs.iter().any(|attribute| attribute.location == 1))
        .expect("the converted vec3 layout");
    assert_eq!(converted.stride, 16, "vec3 is padded to the linked vec4 input");
}

#[test]
fn signed_normalized_conversion_clamps_at_minus_one() {
    // ES 3.0 §2.9.1: signed normalized is max(c / (2^(b-1) - 1), -1), so -128/127 clamps to -1.0 and
    // 127 is exactly 1.0.
    let out = converted_components(3, GL_BYTE, true, &[0x80, 0x7f, 0x00, 0]);
    assert_eq!(&out[..3], &[-1.0, 1.0, 0.0]);
}

/// The formats the IR expresses directly must NOT be converted — that path has to stay byte-identical.
#[test]
fn expressible_formats_are_left_on_the_direct_path() {
    for (size, kind, normalized) in [
        (4, GL_FLOAT, false),
        (4, GL_UNSIGNED_BYTE, true),
        (4, GL_SHORT, true),
    ] {
        let mut c = ctx();
        let mut sink = RecordingSink::with_full_caps();
        setup_tint(&mut c);
        let tint = c.buffers.gen();
        record::bind_buffer(&mut c, GL_ARRAY_BUFFER, tint);
        record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[7u8; 64], 0x88E4);
        record::vertex_attrib_pointer(&mut c, 1, size, kind, normalized, 0, 0);
        record::enable_vertex_attrib(&mut c, 1);
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
        swap::swap_buffers(&mut c, &mut sink).unwrap();
        assert!(
            !sink.batches[0].iter().any(|cmd| matches!(
                cmd,
                Cmd::CreateBuffer(_, desc) if desc.label.starts_with("gl-converted-vertex")
            )),
            "size {size} kind {kind:#x} normalized {normalized} is directly expressible"
        );
    }
}

/// The exact `corpus:attribute_types` short-normalized case, asserted on the EMITTED IR.
///
/// The differential shows Husklet returning `00 22 ff 88` where llvmpipe returns `22 00 88 ff` — the four
/// source shorts read as `{s[1], s[0], s[3], s[2]}`, i.e. the two 16-bit halves of each 32-bit word
/// swapped. This test asks whether that transformation happens in THIS crate: it pins the bytes the driver
/// uploads and the format it declares. If both are right here, the fault is downstream and this test is
/// the evidence for saying so; if either is wrong, it is ours and this test names it.
#[test]
fn short_normalized_uploads_its_bytes_verbatim_and_declares_snorm16x4() {
    // The corpus values, and the little-endian bytes they occupy.
    const SHORTS: [i16; 4] = [4369, -8738, 17476, 32767];
    let vertex = SHORTS
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    let buffer = vertex.repeat(3);

    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let tint = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, tint);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &buffer, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 1, 4, GL_SHORT, true, 0, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // 1. A normalized 4-component short is directly expressible, so it must NOT be converted.
    assert!(
        !batch.iter().any(|cmd| matches!(
            cmd,
            Cmd::CreateBuffer(_, desc) if desc.label.starts_with("gl-converted-vertex")
        )),
        "Snorm16x4 is expressible; the f32 conversion path must not run"
    );

    // 2. The bytes reaching the host are the application's, unaltered.
    let uploaded = batch
        .iter()
        .find_map(|cmd| match cmd {
            Cmd::WriteBuffer { data, .. } if data.len() == buffer.len() => Some(data.clone()),
            _ => None,
        })
        .expect("the vertex buffer upload");
    assert_eq!(
        &uploaded[..8],
        &vertex[..],
        "the driver uploads the source shorts verbatim — no 16-bit reordering happens here"
    );

    // 3. The declared format is `comps | kind<<8 | normalized<<16` = 4 | (4 << 8) | (1 << 16), which the
    //    executor decodes as Snorm16x4. Attribute 1 is at offset 0 of its own slot.
    let attr = pipeline_desc(batch)
        .vertex_buffers
        .iter()
        .flat_map(|layout| layout.attrs.iter())
        .find(|attr| attr.location == 1)
        .expect("the tint attribute");
    assert_eq!(attr.format, 4 | (4 << 8) | (1 << 16), "Snorm16x4");
    assert_eq!(attr.offset, 0);
}

// ---------------------------------------------------------------------------------------------------
// glVertexAttribIPointer delivers INTEGERS, and the pipeline must say so
//
// This is the defect that cost a Chrome session. `glVertexAttribIPointer` routed into the recorder for
// the FLOAT entry point, which hard-coded `integer = false`, so an integer array was indistinguishable
// from `glVertexAttribPointer(GL_UNSIGNED_INT, normalized = FALSE)`. That combination legitimately
// converts to float, so the attribute was converted and declared `Float32x2` while the shader's `uvec2`
// input required `Uint32x2`, and wgpu refused the pipeline:
//
//     In Device::create_render_pipeline, label = 'hl-render'
//       Error matching ShaderStages(VERTEX) shader requirements against the pipeline
//         Location[2] Uint32x2 interpolated as Some(Flat) ... is not provided by the previous stage outputs
//           Input type is not compatible with the provided Float32x2
//
// FAIL-FIRST: `vertex_attrib_ipointer` was landed passing `integer = false` — the rule deliberately
// absent — and this test was watched failing with kind=5 comps=2 but the integer bit CLEAR and a
// `gl-converted-vertex` buffer present. The flag was then set and it passed. The refusal is therefore
// known to come from the flag and not from some other part of the path.
//
// `an_unnormalized_integer_attribute_becomes_its_plain_numeric_value` above is the control that must
// keep passing: it drives `glVertexAttribPointer` with an integer type, where GL genuinely does convert
// to float. If that test breaks, this fix has widened past the entry point it belongs to.
// ---------------------------------------------------------------------------------------------------

/// Lower a draw whose location-1 array was declared with `glVertexAttribIPointer`, and return
/// `(the packed format the pipeline declared, whether a float-conversion buffer was emitted)`.
fn ipointer_format(size: i32, kind: u32) -> (u32, bool) {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let ints = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, ints);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 64], 0x88E4);
    record::vertex_attrib_ipointer(&mut c, 1, size, kind, 0, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());

    let converted = sink.batches[0].iter().any(|cmd| {
        matches!(cmd, Cmd::CreateBuffer(_, desc) if desc.label.starts_with("gl-converted-vertex"))
    });
    let pipe = sink.batches[0]
        .iter()
        .find_map(|cmd| match cmd {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("the positive control must build a pipeline; the assertions are vacuous otherwise");
    let format = pipe
        .vertex_buffers
        .iter()
        .flat_map(|vb| vb.attrs.iter())
        .find(|a| a.location == 1)
        .expect("location 1 must appear in some vertex slot")
        .format;
    (format, converted)
}

#[test]
fn narrow_signed_ipointer_widens_and_sign_extends_with_gl_defaults() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let ints = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, ints);
    let values = [-2i16, 7, 9];
    let bytes = values.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<_>>();
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &bytes, 0x88E4);
    record::vertex_attrib_ipointer(&mut c, 1, 1, GL_SHORT, 0, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let data = batch
        .iter()
        .zip(batch.iter().skip(1))
        .find_map(|(create, write)| match (create, write) {
            (Cmd::CreateBuffer(_, desc), Cmd::WriteBuffer { data, .. })
                if desc.label.starts_with("gl-converted-vertex") => Some(data),
            _ => None,
        })
        .expect("converted integer upload");
    let words = data
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(&words[..4], &[(-2i32) as u32, 0, 0, 1]);
    let attr = pipeline_desc(batch)
        .vertex_buffers
        .iter()
        .flat_map(|layout| &layout.attrs)
        .find(|attr| attr.location == 1)
        .unwrap();
    assert_eq!(attr.format & 0xffff, 4 | (6 << 8), "Sint32x4");
}

#[test]
fn narrow_unsigned_ipointer_zero_extends_and_pads_w() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_tint(&mut c);
    let ints = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, ints);
    let bytes = [255u8, 128, 7].repeat(3);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &bytes, 0x88E4);
    record::vertex_attrib_ipointer(&mut c, 1, 3, GL_UNSIGNED_BYTE, 0, 0);
    record::enable_vertex_attrib(&mut c, 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let data = batch
        .iter()
        .zip(batch.iter().skip(1))
        .find_map(|(create, write)| match (create, write) {
            (Cmd::CreateBuffer(_, desc), Cmd::WriteBuffer { data, .. })
                if desc.label.starts_with("gl-converted-vertex") => Some(data),
            _ => None,
        })
        .unwrap();
    let words = data
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(&words[..4], &[255, 128, 7, 1]);
    let attr = pipeline_desc(batch)
        .vertex_buffers
        .iter()
        .flat_map(|layout| &layout.attrs)
        .find(|attr| attr.location == 1)
        .unwrap();
    assert_eq!(attr.format & 0xffff, 4 | (5 << 8), "Uint32x4");
}

#[test]
fn an_ipointer_attribute_is_declared_integer_and_never_converted() {
    let (format, converted) = ipointer_format(4, GL_UNSIGNED_INT);

    // Read the packing back through explicit shifts rather than by rebuilding it with the same helper
    // that wrote it — a generator checked against itself agrees with itself.
    // `comps | (kind << 8) | (normalized << 16) | (integer << 17)`, kind 5 = u32.
    assert_eq!(format & 0xFF, 4, "four components");
    assert_eq!((format >> 8) & 0xFF, 5, "GL_UNSIGNED_INT is kind 5");
    assert_eq!(
        (format >> 16) & 1,
        0,
        "glVertexAttribIPointer is never normalized"
    );
    assert_eq!(
        (format >> 17) & 1,
        1,
        "the INTEGER bit is the entire reason this entry point exists: without it the pipeline \
         declares Float32x2 and wgpu refuses the uvec2 shader input"
    );
    assert!(
        !converted,
        "an integer array must be handed over raw; converting it to f32 is what produced Float32x2"
    );
}

#[test]
fn a_signed_ipointer_attribute_is_declared_integer_too() {
    // The sibling type, so the fix is not keyed to one enum. kind 6 = i32.
    let (format, converted) = ipointer_format(4, GL_INT);
    assert_eq!(format & 0xFF, 4);
    assert_eq!((format >> 8) & 0xFF, 6, "GL_INT is kind 6");
    assert_eq!((format >> 17) & 1, 1, "signed integers are integers too");
    assert!(!converted);
}
