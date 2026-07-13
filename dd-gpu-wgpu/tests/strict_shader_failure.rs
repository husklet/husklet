#![cfg(target_os = "macos")]

use dd_gpu::{replay, GpuError};
use dd_shim_vk::ir_seam::create_shader_module;

#[test]
fn vulkan_shader_translation_failure_never_falls_back_to_builtin_rendering() {
    let Ok(mut backend) = dd_gpu_wgpu::WgpuBackend::new() else {
        return; // No Metal adapter on this host; the pure translation test still covers the parser.
    };

    // Correct SPIR-V magic enters the strict translation path, but the truncated instruction stream
    // is invalid. Drive the real producer, shared wire codec, and executor replay boundary.
    let cmd = create_shader_module(7, vec![0x0723_0203, 0x0001_0000, 0, 2, 0, 0xffff_ffff]);
    let bytes = dd_gpu::ir::encode_stream(&[cmd]);
    let error = replay::replay_stream(&mut backend, &bytes).expect_err("translation must NACK");
    assert_eq!(error, GpuError::Invalid("SPIR-V shader translation failed"));

    // Cached failures retain the same typed error; they never become a successful builtin module.
    let decoded = dd_gpu::ir::decode_stream(&bytes).expect("wire remains decodable");
    let error = replay::replay(&mut backend, &decoded).expect_err("cached translation must NACK");
    assert_eq!(error, GpuError::Invalid("SPIR-V shader translation failed"));
}
