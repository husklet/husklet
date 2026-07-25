use super::*;

// (8) CAPABILITY HONESTY — the advertisement must match what the executor actually accepts
// =================================================================================================

#[test]
fn capability_advertisement_is_honest() {
    let g = exec();
    let caps = g.capabilities();

    // Present kinds: only Shm is claimed (no IOSurface/dma-buf handoff from this backend).
    assert_eq!(caps.present_kinds, vec![PresentKind::Shm]);

    // Shader payloads: exactly the ones with a real accept path. SPIR-V / GLSL / KERNEL are exercised
    // end-to-end by the tests above; WGSL and MSL have NO wire path this executor accepts, so advertising
    // them would be a lie.
    assert_ne!(
        caps.shader_payloads & shader_payload::SPIRV,
        0,
        "SPIR-V advertised + accepted"
    );
    assert_ne!(
        caps.shader_payloads & shader_payload::GLSL,
        0,
        "GLSL advertised + accepted"
    );
    assert_ne!(
        caps.shader_payloads & shader_payload::KERNEL,
        0,
        "kernel advertised + accepted"
    );
    assert_eq!(
        caps.shader_payloads & shader_payload::WGSL,
        0,
        "WGSL must NOT be advertised (no accept path)"
    );
    assert_eq!(
        caps.shader_payloads & shader_payload::MSL,
        0,
        "MSL must NOT be advertised (rejected)"
    );

    assert!(caps.supports_compute && caps.supports_graphics);
    assert!(
        !caps.supports_timeline_fences,
        "fences are emulated via submit completion, not real timelines"
    );

    // Command set: the ops with a real replay arm are advertised — including BLIT_TEXTURE (scaled/filtered,
    // resampled by a textured-triangle draw) and RESOLVE_TEXTURE (multisample averaging, a zero-draw
    // render-pass resolve), so a negotiation can never promise a command the executor drops.
    for &t in &[
        etag::BEGIN_RENDER_PASS,
        etag::DRAW,
        etag::DRAW_INDEXED,
        etag::DISPATCH,
        etag::CLEAR_RECT,
        etag::COPY_B2B,
        etag::COPY_B2T,
        etag::COPY_T2B,
        etag::COPY_T2T,
        etag::BLIT_TEXTURE,
        etag::RESOLVE_TEXTURE,
        etag::FILL_BUFFER,
        etag::SET_VERTEX_BUFFER,
        etag::SET_INDEX_BUFFER,
        etag::SET_SCISSOR,
        etag::SET_VIEWPORT,
        etag::SET_BLEND_CONSTANT,
    ] {
        assert!(
            caps.supports_command(t),
            "etag {t} has a replay arm and must be advertised"
        );
    }
}
