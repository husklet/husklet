//! Lowering/unit tests for the GLES3.0/3.1 op families filled in this pass: compute dispatch, sync
//! objects, indexed (UBO/SSBO) buffer bindings, PBO-style buffer mapping, and MRT draw/read buffers.
//!
//! Like `lowering.rs`, these drive the shared `hl_gl` services against a `hl_gpu::RecordingSink` and
//! assert the exact protocol `Cmd`/`Enc` stream (no socket, no GPU).

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{compute, map, record, sync};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BindResource;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{Cmd, FenceId, RecordingSink};

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 320, height: 240 };
    c
}

/// The encoder ops of the single `Cmd::Submit` in a batch.
fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch: {batch:?}");
}

const CS: &str = "#version 310 es\nlayout(local_size_x=1) in;\nlayout(std430, binding=0) buffer B { uint v[]; };\nvoid main(){ v[gl_GlobalInvocationID.x] += 1u; }\n";

/// Build + bind a linked compute program with one SSBO at binding 0.
fn setup_compute(c: &mut GlContext) {
    let cs = record::create_shader(c, GL_COMPUTE_SHADER);
    record::shader_source(c, cs, CS);
    record::compile_shader(c, cs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, cs);
    assert!(record::link_program(c, prog));
    assert!(c.programs.program(prog).unwrap().is_compute(), "a compute program links to compute_ir");
    record::use_program(c, prog);

    // An SSBO with data, bound at indexed slot 0.
    let ssbo = record::gen_buffer(c);
    record::bind_buffer(c, GL_SHADER_STORAGE_BUFFER, ssbo);
    record::buffer_data(c, GL_SHADER_STORAGE_BUFFER, &[7u8; 32], 0x88E9);
    record::bind_buffer_base(c, GL_SHADER_STORAGE_BUFFER, 0, ssbo);
}

// ---------------------------------------------------------------------------------------------------
// compute (GLES3.1): glDispatchCompute → CreateComputePipeline + Dispatch
// ---------------------------------------------------------------------------------------------------

#[test]
fn dispatch_compute_emits_compute_pipeline_and_dispatch() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_compute(&mut c);

    compute::dispatch_compute(&mut c, &mut sink, 4, 2, 1).unwrap();
    assert_eq!(sink.batches.len(), 1, "a dispatch submits exactly one batch immediately");
    let batch = &sink.batches[0];

    // The compute program lowers to a CreateShader + a CreateComputePipeline.
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateShader { .. })), "compute shader created");
    let pipe = batch.iter().find_map(|c| match c {
        Cmd::CreateComputePipeline(id, d) => Some((*id, d.clone())),
        _ => None,
    });
    let (pipe_id, pipe_desc) = pipe.expect("a CreateComputePipeline");
    assert_eq!(pipe_desc.compute.entry, "cmain", "the compute pipeline binds the cmain entry");

    // The SSBO becomes a STORAGE buffer + a bind-group buffer entry at binding 0.
    assert!(
        batch.iter().any(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage & buffer_usage::STORAGE != 0)),
        "the SSBO is created with STORAGE usage"
    );
    let bg = batch.iter().find_map(|c| match c {
        Cmd::CreateBindGroup(_, d) => Some(d.clone()),
        _ => None,
    });
    let bg = bg.expect("a CreateBindGroup");
    assert!(
        bg.entries.iter().any(|e| e.binding == 0 && matches!(e.resource, BindResource::Buffer { .. })),
        "the SSBO is bound at binding 0"
    );

    // The compute pass: SetPipeline(pipe) + Dispatch{4,2,1} inside Begin/EndComputePass.
    let ops = submit_ops(batch);
    assert!(matches!(ops.first(), Some(Enc::BeginComputePass)));
    assert!(ops.iter().any(|e| matches!(e, Enc::SetPipeline(p) if *p == pipe_id)));
    assert!(
        ops.iter().any(|e| matches!(e, Enc::Dispatch { x: 4, y: 2, z: 1 })),
        "the grid lowers into a Dispatch: {ops:?}"
    );
    assert!(matches!(ops.last(), Some(Enc::EndComputePass)));
}

#[test]
fn dispatch_without_compute_program_is_an_error_and_submits_nothing() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // No program bound (cur_prog == 0) → GL_INVALID_OPERATION, nothing submitted.
    compute::dispatch_compute(&mut c, &mut sink, 1, 1, 1).unwrap();
    assert!(sink.batches.is_empty());
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

// ---------------------------------------------------------------------------------------------------
// sync objects: glFenceSync signals a fence; glClientWaitSync waits it
// ---------------------------------------------------------------------------------------------------

#[test]
fn fence_sync_signals_and_client_wait_waits_the_ir_fence() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    let token = sync::fence_sync(&mut c, &mut sink, GL_SYNC_GPU_COMMANDS_COMPLETE, 0).expect("a GLsync");
    assert!(sync::is_sync(&c, token));

    // The fence was created + signalled at timeline value 1 by a submitted command buffer.
    let batch = &sink.batches[0];
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateFence(_))), "the IR fence is created");
    let signal = batch.iter().find_map(|c| match c {
        Cmd::Submit(cb) => cb.signal,
        _ => None,
    });
    let (fence, value) = signal.expect("a Submit that signals the fence");
    assert_eq!(value, 1, "the first fence sync signals timeline value 1");

    // Before waiting the fence reads unsignaled.
    assert_eq!(sync::get_synciv(&mut c, token, GL_SYNC_STATUS), Some(GL_UNSIGNALED as i32));

    // A client wait (with the flush bit) blocks on that fence value + returns satisfied.
    let r = sync::client_wait_sync(&mut c, &mut sink, token, GL_SYNC_FLUSH_COMMANDS_BIT, 0);
    assert_eq!(r, GL_CONDITION_SATISFIED);
    assert_eq!(sink.waits, vec![(FenceId(fence), 1)], "the wait targets the signalled fence value");

    // Now it reads signalled, and a second wait short-circuits to ALREADY_SIGNALED.
    assert_eq!(sync::get_synciv(&mut c, token, GL_SYNC_STATUS), Some(GL_SIGNALED as i32));
    assert_eq!(
        sync::client_wait_sync(&mut c, &mut sink, token, 0, 0),
        GL_ALREADY_SIGNALED
    );

    // Deleting drops it; a bad condition is rejected.
    sync::delete_sync(&mut c, token);
    assert!(!sync::is_sync(&c, token));
    assert!(sync::fence_sync(&mut c, &mut sink, 0, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
}

// ---------------------------------------------------------------------------------------------------
// indexed buffer bindings: glBindBufferBase / glBindBufferRange
// ---------------------------------------------------------------------------------------------------

#[test]
fn bind_buffer_base_and_range_record_the_indexed_binding() {
    let mut c = ctx();

    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, 2, 5);
    let b = record::indexed_buffer_binding(&c, GL_UNIFORM_BUFFER, 2).expect("a UBO binding at index 2");
    assert_eq!((b.buffer, b.offset, b.size), (5, 0, 0), "base binds the whole buffer");

    record::bind_buffer_range(&mut c, GL_SHADER_STORAGE_BUFFER, 1, 9, 16, 64);
    let b = record::indexed_buffer_binding(&c, GL_SHADER_STORAGE_BUFFER, 1).expect("an SSBO range at index 1");
    assert_eq!((b.buffer, b.offset, b.size), (9, 16, 64));

    // Unbinding (buffer 0) clears the slot.
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, 2, 0);
    assert!(record::indexed_buffer_binding(&c, GL_UNIFORM_BUFFER, 2).is_none());

    // A non-indexed target is GL_INVALID_ENUM; an out-of-range index is GL_INVALID_VALUE.
    record::bind_buffer_base(&mut c, GL_ARRAY_BUFFER, 0, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, MAX_UNIFORM_BUFFER_BINDINGS, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

// ---------------------------------------------------------------------------------------------------
// buffer mapping: glMapBufferRange + write + glUnmapBuffer flushes a WriteBuffer
// ---------------------------------------------------------------------------------------------------

#[test]
fn map_write_unmap_flushes_a_write_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    let buf = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, buf);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);

    // Map [8, 8+4), write four bytes through the buffer's storage (as the app would via the pointer).
    let (name, off) = map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 8, 4, 0).expect("a mapped range");
    assert_eq!((name, off), (buf, 8));
    c.buffers.get_mut(buf).unwrap().data[off..off + 4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    // Unmap flushes the mapped range to the device as a WriteBuffer of the written bytes.
    assert_eq!(map::unmap_buffer(&mut c, &mut sink, GL_ARRAY_BUFFER).unwrap(), GL_TRUE as u8);
    let batch = &sink.batches[0];
    let wb = batch.iter().find_map(|c| match c {
        Cmd::WriteBuffer { offset, data, .. } => Some((*offset, data.clone())),
        _ => None,
    });
    let (offset, data) = wb.expect("a WriteBuffer flushed on unmap");
    assert_eq!(offset, 8, "the flush lands at the mapped offset");
    assert_eq!(data, vec![0xDE, 0xAD, 0xBE, 0xEF], "the written bytes are flushed");

    // A second unmap with nothing mapped is GL_FALSE + GL_INVALID_OPERATION.
    assert_eq!(map::unmap_buffer(&mut c, &mut sink, GL_ARRAY_BUFFER).unwrap(), GL_FALSE as u8);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

// ---------------------------------------------------------------------------------------------------
// MRT: glDrawBuffers / glReadBuffer
// ---------------------------------------------------------------------------------------------------

#[test]
fn draw_buffers_and_read_buffer_record_and_validate() {
    let mut c = ctx();

    record::draw_buffers(&mut c, &[GL_COLOR_ATTACHMENT0, GL_NONE, GL_COLOR_ATTACHMENT1]);
    assert_eq!(c.draw_buffers, vec![GL_COLOR_ATTACHMENT0, GL_NONE, GL_COLOR_ATTACHMENT1]);

    record::read_buffer(&mut c, GL_COLOR_ATTACHMENT1);
    assert_eq!(c.read_buffer_src, GL_COLOR_ATTACHMENT1);

    // An invalid selector is GL_INVALID_ENUM and leaves state unchanged.
    record::read_buffer(&mut c, GL_ARRAY_BUFFER);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(c.read_buffer_src, GL_COLOR_ATTACHMENT1);
}

// ---------------------------------------------------------------------------------------------------
// glFlushMappedBufferRange: an explicit sub-range flush of a still-mapped buffer → a WriteBuffer
// ---------------------------------------------------------------------------------------------------

#[test]
fn flush_mapped_range_flushes_a_subrange_while_still_mapped() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    let buf = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, buf);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);

    // Map [4, 4+16) then write through the buffer's storage as the app would via the pointer.
    let (name, off) = map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 4, 16, 0).expect("a mapped range");
    assert_eq!((name, off), (buf, 4));
    c.buffers.get_mut(buf).unwrap().data[off..off + 8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

    // Flush the FIRST 8 bytes of the mapping (relative offset 0, length 8) — still mapped, no unmap.
    map::flush_mapped_range(&mut c, &mut sink, GL_ARRAY_BUFFER, 0, 8).unwrap();
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    let batch = &sink.batches[0];
    let (offset, data) = batch
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { offset, data, .. } => Some((*offset, data.clone())),
            _ => None,
        })
        .expect("a WriteBuffer emitted by the explicit flush");
    assert_eq!(offset, 4, "the flush lands at the mapped base offset (map_off 4 + relative 0)");
    assert_eq!(data, vec![1, 2, 3, 4, 5, 6, 7, 8], "exactly the flushed sub-range bytes are uploaded");

    // A negative range is GL_INVALID_VALUE; flushing an unmapped buffer is GL_INVALID_OPERATION.
    map::flush_mapped_range(&mut c, &mut sink, GL_ARRAY_BUFFER, -1, 4).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    let _ = map::unmap_buffer(&mut c, &mut sink, GL_ARRAY_BUFFER);
    map::flush_mapped_range(&mut c, &mut sink, GL_ARRAY_BUFFER, 0, 4).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}

// ---------------------------------------------------------------------------------------------------
// glDispatchComputeIndirect: the grid is read from the bound GL_DISPATCH_INDIRECT_BUFFER → Dispatch
// ---------------------------------------------------------------------------------------------------

#[test]
fn dispatch_compute_indirect_reads_grid_from_buffer_and_dispatches() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_compute(&mut c);

    // An indirect buffer whose three little-endian u32 group counts are {3,5,7} at byte offset 4.
    let ind = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_DISPATCH_INDIRECT_BUFFER, ind);
    let mut bytes = vec![0u8; 4];
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&5u32.to_le_bytes());
    bytes.extend_from_slice(&7u32.to_le_bytes());
    record::buffer_data(&mut c, GL_DISPATCH_INDIRECT_BUFFER, &bytes, 0x88E4);

    compute::dispatch_compute_indirect(&mut c, &mut sink, 4).unwrap();
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    let ops = submit_ops(&sink.batches[0]);
    assert!(
        ops.iter().any(|e| matches!(e, Enc::Dispatch { x: 3, y: 5, z: 7 })),
        "the indirect group counts lower into a Dispatch{{3,5,7}}: {ops:?}"
    );

    // A misaligned offset is GL_INVALID_VALUE; an out-of-range read (no/short buffer) is INVALID_OPERATION.
    compute::dispatch_compute_indirect(&mut c, &mut sink, 3).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    compute::dispatch_compute_indirect(&mut c, &mut sink, 1024).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
}
