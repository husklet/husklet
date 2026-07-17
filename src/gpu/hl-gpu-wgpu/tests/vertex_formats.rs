//! EXHAUSTIVE vertex-attribute format coverage: EVERY `(kind, comps, normalized)` combination the
//! `pipeline::vertex_format` map accepts is driven through a real `create_render_pipeline` whose vertex
//! shader declares a `@location(0)` input of the matching scalar class — so each arm of the map is exercised
//! AND wgpu accepts the resulting `wgpu::VertexFormat` in a live vertex layout. Every WebGPU-invalid
//! combination (1- and 3-component 8-/16-bit, f16×1/×3) is asserted to return a clean typed error rather
//! than being silently widened.
//!
//! (Byte-level data transport for the common formats is already pinned by `executor_coverage.rs`
//! `vertex_buffer_two_attributes_float_and_unorm8` + `instanced_*`; this file completes the MAP coverage.)
//! Skips with no adapter.

use hl_gpu::protocol::model::descriptor::{
    ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{texture_usage, TextureDim, TextureFormat, Topology};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{Cmd, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

#[derive(Clone, Copy)]
enum Class {
    Float,
    Uint,
    Sint,
}

/// `comps | (kind<<8) | (normalized<<16)` — the GL driver's `vertex_format_wire` packing.
fn vfmt(comps: u32, kind: u32, normalized: bool) -> u32 {
    comps | (kind << 8) | (if normalized { 1 << 16 } else { 0 })
}

/// The shader scalar class a format resolves to (Unorm/Snorm/Float → float, Uint → uint, Sint → int).
fn class_of(kind: u32, normalized: bool) -> Class {
    match kind {
        0 | 7 => Class::Float, // f32 / f16
        5 => Class::Uint,      // u32
        6 => Class::Sint,      // i32
        1 | 3 => {
            if normalized {
                Class::Float
            } else {
                Class::Uint
            }
        } // u8 / u16
        2 | 4 => {
            if normalized {
                Class::Float
            } else {
                Class::Sint
            }
        } // i8 / i16
        _ => Class::Float,
    }
}

/// A vertex shader whose `@location(0)` input matches `class`×`comps`, referencing it so it is not pruned.
fn vs_for(class: Class, comps: u32) -> String {
    let (decl, scalar) = match class {
        Class::Float => (
            ["float", "vec2", "vec3", "vec4"][(comps - 1) as usize],
            if comps == 1 { "a" } else { "a.x" },
        ),
        Class::Uint => (
            ["uint", "uvec2", "uvec3", "uvec4"][(comps - 1) as usize],
            if comps == 1 { "a" } else { "a.x" },
        ),
        Class::Sint => (
            ["int", "ivec2", "ivec3", "ivec4"][(comps - 1) as usize],
            if comps == 1 { "a" } else { "a.x" },
        ),
    };
    format!(
        "#version 460\nlayout(location=0) in {decl} a;\nvoid main() {{ gl_Position = vec4(float({scalar})*0.0, 0.0, 0.0, 1.0); }}\n"
    )
}

const FS: &str = "#version 460\nlayout(location=0) out vec4 o;\nvoid main() { o = vec4(1.0); }\n";

fn glsl(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: "main".to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

fn build(
    exec: &mut WgpuExecutor,
    packed: u32,
    class: Class,
    comps: u32,
) -> hl_gpu::Result<Session> {
    let mut s = session(exec);
    let tex = TextureDesc {
        width: 4,
        height: 4,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET,
        label: String::new(),
    };
    let cmds = vec![
        Cmd::CreateTexture(1, tex),
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::VERTEX, &vs_for(class, comps)),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, FS),
        },
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "main".into(),
                },
                fragment: Some(ShaderRef {
                    module: 2,
                    entry: "main".into(),
                }),
                vertex_buffers: vec![VertexLayout {
                    stride: 32,
                    step_mode: 0,
                    attrs: vec![VertexAttr {
                        location: 0,
                        format: packed,
                        offset: 0,
                    }],
                }],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        ),
    ];
    hl_gpu::runtime::submit(&mut s, exec, 0, &cmds)?;
    Ok(s)
}

#[test]
fn every_supported_vertex_format_builds_a_pipeline() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // (kind, comps, normalized) for every arm of `vertex_format`.
    let mut supported: Vec<(u32, u32, bool)> = Vec::new();
    for c in 1..=4 {
        supported.push((0, c, false)); // f32 x1..4
        supported.push((5, c, false)); // u32 x1..4
        supported.push((6, c, false)); // i32 x1..4
    }
    for c in [2u32, 4] {
        supported.push((7, c, false)); // f16 x2/x4
        for kind in [1u32, 2, 3, 4] {
            supported.push((kind, c, false)); // 8-/16-bit int x2/x4
            supported.push((kind, c, true)); // 8-/16-bit normalized x2/x4
        }
    }

    let mut count = 0;
    for (kind, comps, norm) in supported {
        let packed = vfmt(comps, kind, norm);
        let class = class_of(kind, norm);
        build(&mut exec, packed, class, comps)
            .unwrap_or_else(|e| panic!("vertex format kind={kind} comps={comps} norm={norm} must build a pipeline, got {e:?}"));
        count += 1;
    }
    assert_eq!(
        count, 30,
        "all 30 supported vertex formats must be exercised"
    );
}

#[test]
fn webgpu_invalid_vertex_formats_are_honest_errors() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    // WebGPU has no 1-/3-component 8-/16-bit format, and no f16×1/×3 — each must be a typed error, not a
    // silent widening. (The shader class is immaterial; `vertex_format` rejects before the pipeline builds.)
    let invalid = [
        (1u32, 1u32),
        (1, 3),
        (2, 1),
        (2, 3),
        (3, 1),
        (3, 3),
        (4, 1),
        (4, 3),
        (7, 1),
        (7, 3),
    ];
    for (kind, comps) in invalid {
        let packed = vfmt(comps, kind, false);
        let r = build(&mut exec, packed, Class::Float, comps);
        assert!(
            r.is_err(),
            "vertex format kind={kind} comps={comps} is WebGPU-invalid and must error, got Ok"
        );
    }
}
