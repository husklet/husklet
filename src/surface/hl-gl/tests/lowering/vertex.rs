use super::*;

// GskGpu-style vertex-pulling instanced draw: position comes from `gl_VertexID` (no per-vertex
// attribute) and the real data is PER-INSTANCE, drawn out of one big frame VBO. With the GL
// `base-instance` feature unavailable GskGpu BAKES the per-instance region base (`first_instance *
// stride`) into the `glVertexAttribPointer` offset, so an attribute's GL offset can be far larger than
// one stride (here instance 542, stride 48 → offset 26016 for `in_rect`, 26032 for `in_color`). wgpu
// rejects a pipeline whose attribute offset exceeds the vertex-buffer `array_stride`; the lowering must
// hoist the whole-stride base into the vertex-buffer BIND offset, leaving each attribute's in-stride
// offset in `[0, stride)`. Before the fix the layout emitted the raw 26016/26032 offset (NACK); after,
// the stride is 48 with attribute offsets 0/16 and the base rides `SetVertexBuffer { offset }`.
#[test]

fn gsk_vertex_pulling_instance_offset_is_hoisted_into_the_bind_offset() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // One big instance VBO: 546 instances * 48 bytes/instance (>= (542 + 4) instances the draw fetches).
    const STRIDE: i32 = 48;
    const BASE_INSTANCE: i32 = 542;
    const BASE_OFF: i32 = BASE_INSTANCE * STRIDE; // 26016 — the baked region base for the first attribute
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(
        &mut c,
        GL_ARRAY_BUFFER,
        &vec![0u8; ((BASE_INSTANCE + 4) * STRIDE) as usize],
        0x88E4,
    );

    // in_rect @ field 0, in_color @ field 16 — both per-instance (divisor 1), offsets baked with the base.
    record::vertex_attrib_pointer(&mut c, 0, 4, GL_FLOAT, false, STRIDE, BASE_OFF as usize);
    record::vertex_attrib_divisor(&mut c, 0, 1);
    record::enable_vertex_attrib(&mut c, 0);
    record::vertex_attrib_pointer(
        &mut c,
        1,
        4,
        GL_FLOAT,
        false,
        STRIDE,
        (BASE_OFF + 16) as usize,
    );
    record::vertex_attrib_divisor(&mut c, 1, 1);
    record::enable_vertex_attrib(&mut c, 1);

    // A minimal linked program so the draw lowers (the layout comes from the recorded attrib state).
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "attribute vec4 in_rect;\nattribute vec4 in_color;\nvarying vec4 vc;\n\
         void main(){ vc = in_color; gl_Position = in_rect; }\n",
    );
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(
        &mut c,
        fs,
        "precision mediump float;\nvarying vec4 vc;\nvoid main(){ gl_FragColor = vc; }\n",
    );
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);

    record::viewport(&mut c, [0, 0, 640, 480]);
    record::draw_arrays_instanced(&mut c, GL_TRIANGLE_STRIP, 0, 4, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The instanced VBO slot: stride 48, per-instance step, EVERY attribute offset within the stride.
    let pipe = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("CreateRenderPipeline");
    let vl = &pipe.vertex_buffers[0];
    assert_eq!(
        vl.stride, STRIDE as u32,
        "the instance stride is the packed instance size, not the region base"
    );
    assert_eq!(
        vl.step_mode, 1,
        "a divisor-1 attribute makes the slot per-instance"
    );
    for a in &vl.attrs {
        assert!(
            a.offset < vl.stride,
            "attribute at location {} offset {} must lie within the stride {} (wgpu NACKs otherwise)",
            a.location,
            a.offset,
            vl.stride,
        );
    }
    // The hoisted-out field offsets are exactly the in-struct offsets 0 (in_rect) and 16 (in_color).
    let mut offs: Vec<u32> = vl.attrs.iter().map(|a| a.offset).collect();
    offs.sort_unstable();
    assert_eq!(
        offs,
        vec![0, 16],
        "field offsets are recovered relative to the instance region base"
    );

    // The whole-stride region base rides the vertex-buffer bind offset instead.
    let ops = submit_ops(batch);
    let bind_off = ops.iter().find_map(|e| match e {
        Enc::SetVertexBuffer {
            slot: 0, offset, ..
        } => Some(*offset),
        _ => None,
    });
    assert_eq!(
        bind_off,
        Some(BASE_OFF as u64),
        "the baked first_instance*stride base is hoisted to the bind offset"
    );
}

// ---------------------------------------------------------------------------------------------------
// adapter/glsl — GLSL-ES → naga-acceptable desktop GLSL (forwarded, host-compiled)
// ---------------------------------------------------------------------------------------------------
