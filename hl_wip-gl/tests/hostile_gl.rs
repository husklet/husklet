//! Adversarial / hostile robustness sweep of the hl-gl shim (task #189, the fourth in the
//! driver-robustness quartet after the executor / Vulkan / CUDA sweeps).
//!
//! For EACH abuse below we drive a shim entrypoint with malformed input and assert it either sets the
//! correct GL error (`GL_INVALID_ENUM` / `GL_INVALID_VALUE` / `GL_INVALID_OPERATION` /
//! `GL_INVALID_FRAMEBUFFER_OPERATION`) or safely no-ops — but NEVER panics, arithmetic-overflows (debug),
//! or unbounded-allocates — and then a VALID call still works. This is the "shim survives every hostile
//! input and stays usable" gate, complementing the lifecycle-focused `robustness.rs`.
//!
//! Real bugs fixed by this pass (each has a dedicated test named `*_does_not_unbounded_alloc*`):
//!  * `glBufferSubData` with a huge/overflowing offset grew the buffer's `Vec` unbounded (debug-overflow
//!    panic on `offset + len`) — now bounded to the buffer size → `GL_INVALID_VALUE`.
//!  * `glMapBufferRange` with an out-of-range offset/length grew the buffer's `Vec` unbounded — now
//!    bounded to the buffer size → `GL_INVALID_VALUE`.
//!  * `glTexImage2D` with an over-max/empty extent (e.g. 40000×40000, NULL pixels) allocated a multi-GiB
//!    zeroed plane — now rejected beyond `GL_MAX_TEXTURE_SIZE` → `GL_INVALID_VALUE`.
//!  * `glReadPixels` with a huge `w`/`h` allocated a packed region proportional to `w*h*bpp` before any
//!    bounds check — now capped → `GL_INVALID_VALUE`.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{compute, es3, map, query, readpixels, record, sync};
use hl_gpu::RecordingSink;

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 320, height: 240 };
    c
}

const GL_RGBA: u32 = 0x1908;

// ===================================================================================================
// invalid enums → GL_INVALID_ENUM, or a documented safe no-op — never a panic
// ===================================================================================================

/// `glEnable`/`glDisable`/`glBindTexture`/`glTexParameter`/`glBlendFunc` with junk enums must never panic.
/// Unmodeled-but-legal caps (and the untargeted texture target) are honest no-ops (the model tracks only
/// the fixed-function subset it lowers); a following valid call still takes effect.
#[test]
fn junk_enums_to_state_setters_never_panic_and_valid_still_works() {
    let mut c = ctx();
    // A bogus capability is a safe no-op (no error, no state change).
    record::enable(&mut c, 0xDEAD_BEEF);
    record::disable(&mut c, 0x0000_0001);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert!(!c.blend);

    // A bogus glBindTexture target + a junk texture name: no panic, no crash.
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, 0xDEAD, 424242);
    // A junk glBlendFunc factor pair is stored verbatim (validated at lowering); no panic.
    record::blend_func(&mut c, 0xDEAD, 0xBEEF);
    record::tex_parameter(&mut c, 0xDEAD, 0xBEEF);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A valid glEnable(GL_BLEND) still takes effect afterwards.
    record::enable(&mut c, GL_BLEND);
    assert!(c.blend);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// `glDrawBuffers`/`glReadBuffer` reject a non-color enum with `GL_INVALID_ENUM`, then a valid list works.
#[test]
fn draw_read_buffer_reject_bad_enum_then_valid_works() {
    let mut c = ctx();
    record::draw_buffers(&mut c, &[GL_COLOR_ATTACHMENT0, 0xDEAD]);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    record::read_buffer(&mut c, 0xDEAD);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    record::draw_buffers(&mut c, &[GL_COLOR_ATTACHMENT0, GL_NONE]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draw_buffers, vec![GL_COLOR_ATTACHMENT0, GL_NONE]);
    record::read_buffer(&mut c, GL_COLOR_ATTACHMENT0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// `glDrawArrays` with a junk `mode` records the draw (topology validated at lowering) without panicking;
/// a following valid draw is recorded too.
#[test]
fn draw_arrays_with_junk_mode_records_without_panicking() {
    let mut c = ctx();
    record::draw_arrays(&mut c, 0xDEAD_BEEF, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws.len(), 1);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.draws.len(), 2);
}

// ===================================================================================================
// bad / dangling / never-created object names → safe no-op or GL error, never a panic
// ===================================================================================================

/// Binding, using, attaching, linking, and drawing with never-created object names must not panic; a
/// valid object created afterwards still works.
#[test]
fn dangling_object_names_to_bind_use_attach_never_panic() {
    let mut c = ctx();
    // Binding never-created names is a safe no-op (state stores the name; nothing dereferenced).
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, 777);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, 888);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 999);
    record::bind_vertex_array(&mut c, 4242);
    record::use_program(&mut c, 31337);
    // Attaching/linking a never-created program+shader is a graceful failure, not a panic.
    record::attach_shader(&mut c, 31337, 12345);
    assert!(!record::link_program(&mut c, 31337), "linking a phantom program fails cleanly");
    // Drawing with the phantom program bound records the draw; the frame builder drops a program-less draw.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A genuinely-created program then links + binds fine.
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, v, "attribute vec4 p; void main(){ gl_Position = p; }\n");
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, "void main(){ gl_FragColor = vec4(1.0); }\n");
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    assert!(record::link_program(&mut c, p));
    record::use_program(&mut c, p);
    assert_eq!(c.cur_prog, p);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// Deleting never-created object names returns `false` and raises no error (mirrors `glDelete*` on unknown
/// names) — no panic. `glDetachShader` on unknown names is `GL_INVALID_VALUE`.
#[test]
fn deleting_and_detaching_unknown_names_is_safe() {
    let mut c = ctx();
    assert!(!record::delete_buffer(&mut c, 5000));
    assert!(!record::delete_texture(&mut c, 5000));
    assert!(!record::delete_framebuffer(&mut c, 5000));
    assert!(!record::delete_renderbuffer(&mut c, 5000));
    assert!(!record::delete_vertex_array(&mut c, 5000));
    record::delete_program(&mut c, 5000);
    record::delete_shader(&mut c, 5000);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // glDetachShader with phantom names is GL_INVALID_VALUE, no panic.
    record::detach_shader(&mut c, 5000, 6000);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

// ===================================================================================================
// out-of-range indices / units / locations / counts → guarded no-op, never a panic
// ===================================================================================================

/// Out-of-range attribute index, texture unit, and uniform location are guarded no-ops (never index past a
/// fixed array); a valid index afterwards takes effect.
#[test]
fn out_of_range_indices_are_guarded_no_ops() {
    let mut c = ctx();
    // A vertex-attrib index far past MAX_VERTEX_ATTRIBS is a no-op.
    record::vertex_attrib_pointer(&mut c, 9999, 4, GL_FLOAT, false, 0, 0);
    record::vertex_attrib_divisor(&mut c, 9999, 1);
    record::enable_vertex_attrib(&mut c, 9999);
    record::disable_vertex_attrib(&mut c, 9999);
    // A texture unit far past the modeled bank leaves the active unit unchanged.
    record::active_texture(&mut c, GL_TEXTURE0 + 9999);
    assert_eq!(c.active_texture, 0, "an out-of-range unit does not move the active unit");
    // A uniform write to a bogus location on a linked program is a no-op (not a slice panic).
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, v, "attribute vec4 p; void main(){ gl_Position = p; }\n");
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, "void main(){ gl_FragColor = vec4(1.0); }\n");
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    record::link_program(&mut c, p);
    record::use_program(&mut c, p);
    record::uniform_at(&mut c, 99999, &[0u8; 64]);
    record::uniform_sampler(&mut c, 99999, 3);
    record::program_uniform_at(&mut c, p, -7, &[0u8; 16]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A valid attribute index does take effect.
    record::vertex_attrib_pointer(&mut c, 0, 4, GL_FLOAT, false, 0, 0);
    record::enable_vertex_attrib(&mut c, 0);
    assert!(c.attr[0].enabled);
}

/// A huge `glDrawArrays` / `glDrawElements` count (near `i32::MAX`) with only VBO-backed attributes must
/// not overflow or unbounded-allocate at record time (no client-array capture runs) — the draw is just
/// recorded. A negative count is `GL_INVALID_VALUE`.
#[test]
fn huge_draw_counts_do_not_overflow_or_alloc() {
    let mut c = ctx();
    // No enabled client-side attributes → no per-vertex capture; a huge count is recorded verbatim.
    record::draw_arrays(&mut c, GL_TRIANGLES, i32::MAX - 1, i32::MAX);
    record::draw_elements(&mut c, GL_TRIANGLES, i32::MAX, GL_UNSIGNED_SHORT, 0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws.len(), 2);
    // A negative count is rejected, recording nothing.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, -5);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(c.draws.len(), 2);
}

/// Negative / huge `glViewport` + `glScissor` dimensions are stored without panicking; a valid viewport
/// afterwards is stored too. (The frame builder clamps at lowering; the record op never faults.)
#[test]
fn extreme_viewport_and_scissor_dims_do_not_panic() {
    let mut c = ctx();
    record::viewport(&mut c, [-1, -1, i32::MAX, i32::MAX]);
    record::scissor(&mut c, [i32::MIN, i32::MIN, -4, -4]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    record::viewport(&mut c, [0, 0, 320, 240]);
    assert_eq!(c.viewport, [0, 0, 320, 240]);
}

// ===================================================================================================
// oversized dims / out-of-range ranges → bounded, GL_INVALID_VALUE, NEVER an unbounded alloc
// ===================================================================================================

/// BUG (fixed): `glTexImage2D` with an over-max (or empty-pixel) extent allocated a multi-GiB zeroed
/// plane. Now an extent beyond `GL_MAX_TEXTURE_SIZE` (or negative) is `GL_INVALID_VALUE` before any
/// allocation; a within-limits upload still works.
#[test]
fn tex_image_2d_oversized_extent_does_not_unbounded_alloc() {
    let mut c = ctx();
    let t = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);

    // 40000×40000 RGBA8 = 6.4 GiB of zeroed storage — must be rejected, not allocated.
    record::tex_image_2d(&mut c, 40000, 40000, &[]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(c.textures.get(t).unwrap().data.is_empty(), "no oversized plane was materialized");
    // A negative extent is also rejected.
    record::tex_image_2d(&mut c, -1, 16, &[]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // A within-limits upload still works.
    record::tex_image_2d(&mut c, 4, 4, &[0xABu8; 64]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.textures.get(t).unwrap().data.len(), 64);
}

/// BUG (fixed): `glBufferSubData` with a huge/overflowing offset grew the buffer `Vec` unbounded (or
/// panicked on `offset + len`). Now an out-of-range range is `GL_INVALID_VALUE` and the buffer is
/// untouched; an in-range write still works.
#[test]
fn buffer_sub_data_out_of_range_does_not_unbounded_alloc() {
    let mut c = ctx();
    let b = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, b);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[7u8; 32], 0);

    // A near-usize::MAX offset must not grow the buffer to a multi-exabyte Vec (or overflow the add).
    record::buffer_sub_data(&mut c, GL_ARRAY_BUFFER, usize::MAX - 3, &[1u8; 8]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A range that reaches just past the end is rejected too, leaving the buffer at its original size.
    record::buffer_sub_data(&mut c, GL_ARRAY_BUFFER, 30, &[1u8; 8]);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(c.buffers.get(b).unwrap().data.len(), 32, "buffer size is unchanged after a rejected write");
    assert_eq!(c.buffers.get(b).unwrap().data, vec![7u8; 32], "bytes are untouched");

    // An in-range write still lands.
    record::buffer_sub_data(&mut c, GL_ARRAY_BUFFER, 8, &[0xEE; 4]);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(&c.buffers.get(b).unwrap().data[8..12], &[0xEE; 4]);
}

/// BUG (fixed): `glMapBufferRange` with an out-of-range offset/length grew the buffer `Vec` unbounded.
/// Now an offset/length beyond the buffer size is `GL_INVALID_VALUE`; an in-range map still works.
#[test]
fn map_buffer_range_out_of_range_does_not_unbounded_alloc() {
    let mut c = ctx();
    let b = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, b);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0);

    // A multi-GiB length must not grow the 32-byte buffer.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 0, 1 << 40, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // An overflowing offset+length is rejected too.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, isize::MAX, 16, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // Just past the end is rejected.
    assert!(map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 30, 4, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert_eq!(c.buffers.get(b).unwrap().data.len(), 32, "buffer size is unchanged after a rejected map");

    // An in-range map still works.
    let mapped = map::map_buffer_range(&mut c, GL_ARRAY_BUFFER, 8, 4, 0);
    assert_eq!(mapped, Some((b, 8)));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// A huge `glBindBufferRange` index, a huge compute dispatch group, and a huge tex-storage extent are all
/// bounded `GL_INVALID_VALUE`s with no allocation (round-out of the record + service size guards).
#[test]
fn oversized_indexed_binding_dispatch_and_storage_are_bounded() {
    let mut c = ctx();
    let b = record::gen_buffer(&mut c);
    record::bind_buffer_base(&mut c, GL_UNIFORM_BUFFER, u32::MAX, b);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    let mut sink = RecordingSink::with_full_caps();
    compute::dispatch_compute(&mut c, &mut sink, u32::MAX, 1, 1).unwrap();
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(sink.batches.is_empty());

    let t = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, t);
    record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, GL_RGBA, i32::MAX, i32::MAX);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

// ===================================================================================================
// glReadPixels — out-of-bounds region / huge extent → bounded, no OOB read, no unbounded alloc
// ===================================================================================================

/// BUG (fixed): `glReadPixels` allocated a packed region proportional to `w*h*bpp` before any bound check.
/// A huge extent is now `GL_INVALID_VALUE` (never allocated); a normal read still works.
#[test]
fn read_pixels_huge_extent_does_not_unbounded_alloc() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // ~2 billion × ~2 billion × 4 bytes — must be rejected, not allocated.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, i32::MAX, i32::MAX, GL_RGBA).unwrap();
    assert!(px.is_empty());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // A normal-sized read of the (empty) default framebuffer returns a zero-filled buffer, no error.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, 4, 4, GL_RGBA).unwrap();
    assert_eq!(px.len(), 4 * 4 * 4);
    assert!(px.iter().all(|&b| b == 0));
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// A `glReadPixels` region reaching outside the render target reads back zeros for the out-of-bounds
/// texels (no OOB slice read, no panic), and a negative width is an empty no-op.
#[test]
fn read_pixels_out_of_bounds_region_is_zero_filled_no_oob() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // A region far outside a (nonexistent) rendered target: bounded to zeros, no OOB read.
    let px = readpixels::read_pixels(&mut c, &mut sink, -100, -100, 8, 8, GL_RGBA).unwrap();
    assert_eq!(px.len(), 8 * 8 * 4);
    assert!(px.iter().all(|&b| b == 0));
    // A non-positive extent is an empty no-op.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, -1, 4, GL_RGBA).unwrap();
    assert!(px.is_empty());
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

// ===================================================================================================
// draw with no program / incomplete framebuffer / mismatched attachment → GL error, no panic
// ===================================================================================================

/// An incomplete framebuffer reports the right completeness status, a blit against it is
/// `GL_INVALID_FRAMEBUFFER_OPERATION`, and a draw against it merely records (no panic).
#[test]
fn incomplete_framebuffer_blit_and_draw_are_safe() {
    let mut c = ctx();
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    // No color attachment yet → INCOMPLETE_MISSING_ATTACHMENT.
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
    );
    // A blit sourcing/targeting the incomplete FBO raises GL_INVALID_FRAMEBUFFER_OPERATION, no panic.
    record::blit_framebuffer(&mut c, 0, 0, 4, 4, 0, 0, 4, 4, GL_COLOR_BUFFER_BIT, GL_NEAREST);
    assert_eq!(c.take_gl_error(), GL_INVALID_FRAMEBUFFER_OPERATION);
    // Drawing with no program + an incomplete FBO bound just records the draw (dropped at lowering).
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws.len(), 1);

    // Attaching a real texture makes it complete.
    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 4, 4, &[0u8; 64]);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    assert_eq!(record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);
}

/// `glFramebufferTexture2D` with a bad `textarget` / unmodeled attachment is `GL_INVALID_VALUE`; with a
/// dangling texture name it is `GL_INVALID_OPERATION`; a valid attach then succeeds.
#[test]
fn framebuffer_texture_2d_bad_attachment_and_dangling_texture() {
    let mut c = ctx();
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    // A non-2D textarget is GL_INVALID_VALUE.
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_3D, 0, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A dangling texture name is GL_INVALID_OPERATION.
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 4242, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // A valid attach succeeds.
    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 4, 4, &[0u8; 64]);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.framebuffers.color_attachment(fbo), tex);
}

// ===================================================================================================
// service ops (sink-touching / es3 object) hostile edges → GL error, no panic
// ===================================================================================================

/// A grab-bag of hostile object-service calls (bad sampler/query/sync/transform-feedback/pipeline names +
/// enums): each sets the right GL error (or safely no-ops) and never panics; a valid call then works.
#[test]
fn hostile_object_service_edges_never_panic() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    // Sampler: unknown name → INVALID_OPERATION; junk pname → INVALID_ENUM.
    es3::sampler_parameter(&mut c, 9090, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    let s = es3::gen_sampler(&mut c);
    es3::sampler_parameter(&mut c, s, 0xDEAD, 0, 0.0);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    // Query: bad target → INVALID_ENUM; unknown id → INVALID_OPERATION.
    es3::begin_query(&mut c, 0xDEAD, 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    es3::begin_query(&mut c, GL_ANY_SAMPLES_PASSED, 777);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // Program pipeline: use-stages on unknown pipeline → INVALID_OPERATION; bad stage bits → INVALID_VALUE.
    es3::use_program_stages(&mut c, 4242, GL_VERTEX_SHADER_BIT, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    let pp = es3::gen_program_pipeline(&mut c);
    es3::use_program_stages(&mut c, pp, 0x8000_0000, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // Sync: junk condition/flags → INVALID_ENUM/INVALID_VALUE; waiting/deleting an unknown sync →
    // INVALID_VALUE and never a panic.
    assert!(sync::fence_sync(&mut c, &mut sink, 0xDEAD, 0).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(sync::client_wait_sync(&mut c, &mut sink, 555, 0, 0), GL_WAIT_FAILED);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    sync::wait_sync(&mut c, &mut sink, 555, 0, GL_TIMEOUT_IGNORED);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    assert!(sync::get_synciv(&mut c, 555, GL_SYNC_STATUS).is_none());
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // Transform feedback: bad varyings program → INVALID_VALUE; junk primitive mode → INVALID_ENUM.
    es3::transform_feedback_varyings(&mut c, 0, vec!["v".into()], GL_INTERLEAVED_ATTRIBS);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    es3::begin_transform_feedback(&mut c, 0xDEAD);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);

    // A valid sampler parameter still sticks afterwards.
    es3::sampler_parameter(&mut c, s, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32, 0.0);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// `glGetIntegerv` / `glGetString` / `glGetStringi` with unrecognized names return a benign fallback
/// (never a null deref / OOB) — an app polling junk enums must not crash the driver.
#[test]
fn junk_query_names_return_benign_fallbacks() {
    let c = ctx();
    let mut out = [0i32; 4];
    assert_eq!(query::get_integerv(&c, 0xDEAD_BEEF, &mut out), 1);
    assert_eq!(out[0], 0);
    // An unrecognized glGetString name is the empty (NUL-terminated) string, never null.
    assert_eq!(query::gl_string(0xDEAD), b"\0");
    // An out-of-range indexed extension query is None (the caller returns a null pointer + spec error).
    assert!(query::string_i(GL_EXTENSIONS, 9999).is_none());
    // Reflection getters on a bogus program are -1 / 0 / None, never a panic.
    assert_eq!(query::uniform_location(&c, 777, "x"), -1);
    assert_eq!(query::attrib_location(&c, 777, "x"), -1);
    assert_eq!(query::get_programiv(&c, 777, GL_LINK_STATUS), 0);
    assert!(query::active_uniform(&c, 777, 0).is_none());
}
