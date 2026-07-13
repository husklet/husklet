//! Versioned capability-handshake behavior + in-crate mirrors of the two tracked backend ledger gates
//! (`backend_capabilities_describe_negotiable_shader_format_and_command_support` and
//! `wgpu_present_capability_has_a_working_present_operation` in `rendering_backends.rs`). Those tracked
//! gates are source-inspection checks; the mirrors here read the same files and assert the same
//! substrings, alongside real serialize/negotiate behavioral tests so the descriptor is proven to work,
//! not just to exist.

use hl_gpu::backend::{
    command_bits, format_bits, shader_payload, Capabilities, FeatureRequest, GpuBackend, ALL_COMMANDS,
};
use hl_gpu::ir::{etag, TextureFormat, WIRE_VERSION};
use hl_gpu::software::SoftwareBackend;
use hl_gpu::GpuError;
use std::path::{Path, PathBuf};

// ---- workspace-root source reader (byte-identical to rendering_backends.rs's `read`) --------------

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}
fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

// ---- MIRROR of `backend_capabilities_describe_negotiable_shader_format_and_command_support` --------

#[test]
fn backend_capabilities_describe_negotiable_shader_format_and_command_support() {
    let source = read("hl-gpu/src/backend.rs");
    for required_field in [
        "wire_version",
        "command_bits",
        "shader_payloads",
        "texture_formats",
        "max_frame_bytes",
        "max_buffer_bytes",
        "max_bind_groups",
        "supports_timeline_fences",
    ] {
        assert!(source.contains(required_field), "Capabilities lacks required negotiation field `{required_field}`");
    }
}

// ---- MIRROR of `wgpu_present_capability_has_a_working_present_operation` ---------------------------

#[test]
fn wgpu_present_capability_has_a_working_present_operation() {
    let source = read("hl-gpu-wgpu/src/backend.rs");
    let advertises = source.contains("present_kinds: vec![PresentKind::IoSurface]");
    let rejects = source.contains("Err(GpuError::Unsupported(\"present (use set_render_target + read_target");
    assert!(
        !(advertises && rejects),
        "WgpuBackend advertises IoSurface presentation while GpuBackend::present always returns Unsupported"
    );
}

// ---- behavioral: the descriptor really serializes + negotiates ------------------------------------

#[test]
fn capabilities_round_trip_through_the_handshake() {
    let caps = SoftwareBackend::new().capabilities();
    let bytes = caps.to_handshake();
    let decoded = Capabilities::from_handshake(&bytes).expect("decode handshake");
    assert_eq!(decoded, caps, "capability descriptor did not survive the serialized handshake");
}

#[test]
fn negotiate_accepts_a_fully_supported_request() {
    let caps = SoftwareBackend::new().capabilities();
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::PTX,
        command_bits: command_bits(&[etag::COPY_T2T, etag::BLIT_TEXTURE, etag::DISPATCH]),
        texture_formats: format_bits(&[TextureFormat::Rgba8Unorm, TextureFormat::Bgra8Unorm]),
    };
    assert_eq!(caps.negotiate(&req), Ok(()));
}

#[test]
fn negotiate_rejects_an_unsupported_shader_payload_cleanly() {
    // The software oracle executes PTX, not MSL. A guest that needs MSL must fail negotiation cleanly.
    let caps = SoftwareBackend::new().capabilities();
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::MSL,
        command_bits: 0,
        texture_formats: 0,
    };
    assert!(matches!(caps.negotiate(&req), Err(GpuError::Unsupported(_))));
    assert!(!caps.supports_shader_payload(shader_payload::MSL));
    assert!(caps.supports_shader_payload(shader_payload::PTX));
}

#[test]
fn negotiate_rejects_an_unsupported_command_tag_without_a_runtime_bad_tag() {
    // A backend advertising every command EXCEPT BlitTexture; a guest requiring BlitTexture must be told
    // NO at negotiation time (a clean Unsupported), not discover it as a runtime BadTag mid-frame.
    let mut caps = SoftwareBackend::new().capabilities();
    caps.command_bits = command_bits(ALL_COMMANDS) & !(1u64 << etag::BLIT_TEXTURE);
    assert!(!caps.supports_command(etag::BLIT_TEXTURE));
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: 0,
        command_bits: command_bits(&[etag::BLIT_TEXTURE]),
        texture_formats: 0,
    };
    let err = caps.negotiate(&req).unwrap_err();
    assert!(matches!(err, GpuError::Unsupported(_)), "expected a clean negotiated Unsupported, got {err:?}");
}

#[test]
fn negotiate_rejects_an_unsupported_texture_format() {
    // The CPU oracle materializes color formats only; a depth-format request fails negotiation.
    let caps = SoftwareBackend::new().capabilities();
    assert!(caps.supports_format(TextureFormat::Rgba8Unorm));
    assert!(!caps.supports_format(TextureFormat::Depth32Float));
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: 0,
        command_bits: 0,
        texture_formats: format_bits(&[TextureFormat::Depth32Float]),
    };
    assert!(matches!(caps.negotiate(&req), Err(GpuError::Unsupported(_))));
}

#[test]
fn negotiate_rejects_a_wire_version_mismatch() {
    let caps = SoftwareBackend::new().capabilities();
    let req = FeatureRequest {
        wire_version: WIRE_VERSION + 1,
        shader_payloads: 0,
        command_bits: 0,
        texture_formats: 0,
    };
    assert!(matches!(caps.negotiate(&req), Err(GpuError::Unsupported(_))));
}

#[test]
fn advertised_capabilities_carry_the_current_wire_version() {
    // The descriptor's wire_version is the single-sourced IR version, so a guest negotiates against the
    // exact tag set the backend decodes.
    assert_eq!(SoftwareBackend::new().capabilities().wire_version, WIRE_VERSION);
}
