use super::*;

#[test]
fn set_viewport_and_scissor_lower_to_encoder_ops() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_set_viewport(d, cb, 0.0, 0.0, 640.0, 480.0, 0.0, 1.0).unwrap();
        // A negative scissor offset clamps to 0 (the IR scissor is unsigned).
        record::cmd_set_scissor(d, cb, 0, 0, 640, 480).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::SetViewport {
                x: 0.0,
                y: 0.0,
                w: 640.0,
                h: 480.0,
                min_depth: 0.0,
                max_depth: 1.0
            },
            Enc::SetScissor {
                x: 0,
                y: 0,
                w: 640,
                h: 480
            },
        ]
    );
}

#[test]
fn push_constants_reach_the_command_buffer_for_the_draw() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Write 8 bytes at offset 0, then overwrite 4 bytes at offset 4 (grows/patches the block in place).
    record::cmd_push_constants(&mut d, cb, 0, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    record::cmd_push_constants(&mut d, cb, 4, &[9, 9, 9, 9]).unwrap();
    // The recorded block is honest command state a draw reads (the IR has no push-constant channel yet).
    assert_eq!(
        d.command_buffers.get(&cb).unwrap().push_constants,
        vec![1, 2, 3, 4, 9, 9, 9, 9]
    );
    // Misaligned / zero-size pushes are typed errors, never a silent partial write.
    assert!(record::cmd_push_constants(&mut d, cb, 2, &[0, 0, 0, 0]).is_err());
    assert!(record::cmd_push_constants(&mut d, cb, 0, &[0, 0, 0]).is_err());
}

#[test]
fn dynamic_state_is_recorded_but_emits_no_encoder_op() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_set_line_width(&mut d, cb, 2.5).unwrap();
    record::cmd_set_depth_bias(&mut d, cb, 1.0, 0.0, 2.0).unwrap();
    record::cmd_set_blend_constants(&mut d, cb, [0.1, 0.2, 0.3, 0.4]).unwrap();
    // FRONT_AND_BACK = 0x3 sets both faces; FRONT = 0x1 sets only the front.
    record::cmd_set_stencil_reference(&mut d, cb, 0x3, 7).unwrap();
    record::cmd_set_stencil_compare_mask(&mut d, cb, 0x1, 0xff).unwrap();
    d.end_command_buffer(cb).unwrap();

    // The state is recorded (observable, honest) …
    let rec = d.command_buffers.get(&cb).unwrap();
    assert_eq!(rec.dynamic.line_width, 2.5);
    assert_eq!(rec.dynamic.depth_bias, (1.0, 0.0, 2.0));
    assert_eq!(rec.dynamic.blend_constants, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(rec.dynamic.stencil_reference, (7, 7));
    assert_eq!(rec.dynamic.stencil_compare_mask, (0xff, 0));
    // … but the software rasterizer models none of it, so no encoder op is emitted.
    assert!(
        rec.enc.is_empty(),
        "dynamic state emits no encoder op, got {:?}",
        rec.enc
    );
}

#[test]
fn indirect_draws_read_args_and_lower_to_direct_draws() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A valid indirect buffer (INDIRECT usage) large enough for two 16-byte VkDrawIndirectCommands,
    // backed by memory the app has filled on the CPU (the mapped-buffer indirect-args pattern).
    let indirect =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let mem = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, indirect, mem, 0).unwrap();
    // cmd0 = {vertexCount:6, instanceCount:2, firstVertex:3, firstInstance:1}
    // cmd1 = {vertexCount:3, instanceCount:1, firstVertex:0, firstInstance:0}
    let mut args = Vec::new();
    for w in [6u32, 2, 3, 1, 3, 1, 0, 0] {
        args.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, mem, 0, &args).unwrap();

    // The indirect draw reads both argument structs and lowers each to the SAME direct Enc::Draw.
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect(d, cb, indirect, 0, 2, 16).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::Draw {
                vertex_count: 6,
                instance_count: 2,
                first_vertex: 3,
                first_instance: 1
            },
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
        ]
    );
    // The equivalent DIRECT draws produce the byte-identical encoder stream (indirect == direct).
    let direct = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw(d, cb, 6, 2, 3, 1).unwrap();
        record::cmd_draw(d, cb, 3, 1, 0, 0).unwrap();
    });
    assert_eq!(
        enc, direct,
        "an indirect draw must lower to its direct twin"
    );

    // vkCmdDrawIndexedIndirect reads the 20-byte struct and lowers to the matching DrawIndexed.
    let idx =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let mem2 = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, idx, mem2, 0).unwrap();
    // {indexCount:9, instanceCount:3, firstIndex:2, vertexOffset:0, firstInstance:5}
    let mut ib = Vec::new();
    for w in [9u32, 3, 2, 0, 5] {
        ib.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, mem2, 0, &ib).unwrap();
    let enc_idx = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indexed_indirect(d, cb, idx, 0, 1, 20).unwrap();
    });
    assert_eq!(
        enc_idx,
        vec![Enc::DrawIndexed {
            index_count: 9,
            instance_count: 3,
            first_index: 2,
            base_vertex: 0,
            first_instance: 5
        }]
    );

    // vkCmdDispatchIndirect reads the 12-byte VkDispatchIndirectCommand{x,y,z} out of the same host-visible
    // backing (the first three words of `args`: 6, 2, 3) and lowers to the SAME compute pass the equivalent
    // vkCmdDispatch(6,2,3) would emit — no pipeline / bind group is bound here, so just the pass wrapper.
    let enc_disp = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_dispatch_indirect(d, cb, indirect, 0).unwrap();
    });
    assert_eq!(
        enc_disp,
        vec![
            Enc::BeginComputePass,
            Enc::Dispatch { x: 6, y: 2, z: 3 },
            Enc::EndComputePass
        ],
        "dispatch-indirect lowers the buffer-sourced workgroup counts to a direct Dispatch"
    );
    // The equivalent DIRECT dispatch produces the byte-identical encoder stream (indirect == direct).
    let direct_disp = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_dispatch(d, cb, 6, 2, 3).unwrap();
    });
    assert_eq!(
        enc_disp, direct_disp,
        "an indirect dispatch must lower to its direct twin"
    );

    // Truthful failure: an unknown buffer, a non-INDIRECT buffer, and an out-of-range span all error.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_draw_indirect(&mut d, cb, 0xdead, 0, 1, 16).is_err());
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    assert!(record::cmd_draw_indirect(&mut d, cb, vbuf, 0, 1, 16).is_err());
    // 5 draws * 16 bytes = 80 > 64: out of bounds.
    assert!(record::cmd_draw_indirect(&mut d, cb, indirect, 0, 5, 16).is_err());
}

#[test]
fn indirect_count_draws_read_count_from_buffer_and_clamp_to_max() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // Argument buffer: three 16-byte VkDrawIndirectCommands, CPU-filled (the mapped indirect-args pattern).
    let indirect =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let amem = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, indirect, amem, 0).unwrap();
    let mut args = Vec::new();
    for w in [6u32, 2, 3, 1, 3, 1, 0, 0, 9, 4, 2, 5] {
        args.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, amem, 0, &args).unwrap();
    // A separate host-visible count buffer holding the GPU/CPU-produced draw count `2`.
    let count =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 16).unwrap();
    let cmem = d.allocate_memory(16).unwrap();
    create::bind_buffer_memory(&mut d, count, cmem, 0).unwrap();
    create::write_mapped(&mut d, cmem, 0, &2u32.to_le_bytes()).unwrap();

    // maxDrawCount = 3, count buffer says 2 → draws exactly the first two argument structs.
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect_count(d, cb, indirect, 0, count, 0, 3, 16).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::Draw { vertex_count: 6, instance_count: 2, first_vertex: 3, first_instance: 1 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
        ],
        "an indirect-count draw reads the count from the buffer and lowers each arg to a direct Draw"
    );

    // maxDrawCount = 1 clamps the buffer's count of 2 down to 1 (spec: actual = min(count, maxDrawCount)).
    let clamped = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect_count(d, cb, indirect, 0, count, 0, 1, 16).unwrap();
    });
    assert_eq!(
        clamped,
        vec![Enc::Draw {
            vertex_count: 6,
            instance_count: 2,
            first_vertex: 3,
            first_instance: 1
        }],
        "maxDrawCount must clamp the buffer-sourced count"
    );

    // vkCmdDrawIndexedIndirectCount reads a 20-byte struct per draw; maxDrawCount 1 clamps to one DrawIndexed.
    let idx =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let imem = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, idx, imem, 0).unwrap();
    let mut ib = Vec::new();
    for w in [9u32, 3, 2, 0, 5] {
        ib.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, imem, 0, &ib).unwrap();
    let enc_idx = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indexed_indirect_count(d, cb, idx, 0, count, 0, 1, 20).unwrap();
    });
    assert_eq!(
        enc_idx,
        vec![Enc::DrawIndexed {
            index_count: 9,
            instance_count: 3,
            first_index: 2,
            base_vertex: 0,
            first_instance: 5
        }]
    );

    // Truthful failure: a count buffer without INDIRECT usage, and an unknown count buffer, both error.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    assert!(record::cmd_draw_indirect_count(&mut d, cb, indirect, 0, vbuf, 0, 3, 16).is_err());
    assert!(record::cmd_draw_indirect_count(&mut d, cb, indirect, 0, 0xdead, 0, 3, 16).is_err());
}

#[test]
fn copy_buffer_v1_and_v2_share_the_same_lowering() {
    // The `vkCmdCopyBuffer2` shim entry point re-parses `VkCopyBufferInfo2` and delegates to this exact
    // `record::cmd_copy_buffer` lowering — so the v2 path lowers identically to v1 (asserted here).
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let (s, t) = (buf_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer(d, cb, src, dst, 8, 16, 32).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToBuffer {
            src: s,
            src_offset: 8,
            dst: t,
            dst_offset: 16,
            size: 32
        }]
    );
}

#[test]
fn pipeline_barrier_records_layout_transition_and_emits_no_ir() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    // VK_IMAGE_LAYOUT_UNDEFINED (0) -> VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL (7).
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_pipeline_barrier(d, cb, &[(img, 0, 7)]).unwrap();
    });
    // The layout-implicit IR carries no encoder op for a barrier.
    assert!(
        enc.is_empty(),
        "a pipeline barrier emits no encoder op, got {enc:?}"
    );
    // The transition is modeled in device bookkeeping.
    assert_eq!(d.image_layouts.get(&img), Some(&7));
}
