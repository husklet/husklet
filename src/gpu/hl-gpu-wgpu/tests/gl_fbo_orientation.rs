//! LINUX-ONLY: this is the one test in this crate that consumes `hl-gl`, whose build script cross-builds
//! the guest EGL/GLES shims and therefore needs a Linux cross-linker. The dev-dependency is scoped to
//! Linux in `Cargo.toml` so the rest of this crate's tests build on a macOS host without that toolchain;
//! this attribute is the other half of that scope, making the file compile to nothing elsewhere.
#![cfg(target_os = "linux")]

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{frame, query, record};
use hl_gpu::protocol::model::descriptor::{FrameSerial, SurfaceToken};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 64;
const H: u32 = 64;

const VS_FLAT: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS_QUADRANTS: &str = r#"
precision mediump float;
void main() {
  bool left = gl_FragCoord.x < 8.0;
  bool bottom = gl_FragCoord.y < 8.0;
  if (bottom && left) gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0);
  else if (bottom) gl_FragColor = vec4(0.0, 0.0, 1.0, 1.0);
  else if (left) gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0);
  else gl_FragColor = vec4(1.0, 1.0, 0.0, 1.0);
}
"#;
const VS_TEXTURE: &str = r#"
attribute vec2 aPos;
attribute vec2 aUV;
varying vec2 vUV;
void main(){ vUV = aUV; gl_Position = vec4(aPos, 0.0, 1.0); }
"#;
const FS_TEXTURE: &str = r#"
precision mediump float;
varying vec2 vUV;
uniform sampler2D uTex;
void main(){ gl_FragColor = texture2D(uTex, vUV); }
"#;

fn program(context: &mut GlContext, vertex: &str, fragment: &str) -> u32 {
    let vs = record::create_shader(context, GL_VERTEX_SHADER);
    record::shader_source(context, vs, vertex);
    record::compile_shader(context, vs);
    let fs = record::create_shader(context, GL_FRAGMENT_SHADER);
    record::shader_source(context, fs, fragment);
    record::compile_shader(context, fs);
    let program = record::create_program(context);
    record::attach_shader(context, program, vs);
    record::attach_shader(context, program, fs);
    assert!(record::link_program(context, program));
    record::use_program(context, program);
    program
}

fn fullscreen(context: &mut GlContext, program: u32, uv: bool) {
    let vertices = [
        -1.0f32, -1.0, 0.0, 0.0, 3.0, -1.0, 2.0, 0.0, -1.0, 3.0, 0.0, 2.0,
    ];
    let bytes = vertices
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    let buffer = context.buffers.gen();
    record::bind_buffer(context, GL_ARRAY_BUFFER, buffer);
    record::buffer_data(context, GL_ARRAY_BUFFER, &bytes, 0x88E4);
    let position = query::attrib_location(context, program, "aPos") as usize;
    let texcoord = query::attrib_location(context, program, "aUV") as usize;
    record::vertex_attrib_pointer(context, position, 2, GL_FLOAT, false, 16, 0);
    record::enable_vertex_attrib(context, position);
    if uv {
        record::vertex_attrib_pointer(context, texcoord, 2, GL_FLOAT, false, 16, 8);
        record::enable_vertex_attrib(context, texcoord);
    }
}

fn pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = ((y * W + x) * 4) as usize;
    // GL's default presentation target is BGRA8; expose RGBA here for readable color assertions.
    [bytes[at + 2], bytes[at + 1], bytes[at], bytes[at + 3]]
}

/// A 1:1 textured composite of an offscreen FBO onto the default framebuffer must present the SAME image
/// the offscreen pass produced. Both are internal render targets and both store rows top-down (see
/// `service::frame::geometry::stores_bottom_up_rows`), so the presented plane equals the atlas plane
/// quadrant for quadrant. The atlas is asymmetric in both axes, so a flip, transpose or 180° rotation
/// anywhere in the chain permutes the quadrants and fails.
///
/// This is the guard that went dead: reflecting clip Y for the default framebuffer made the composite the
/// vertical MIRROR of the atlas it copies, which is what mirrored every presented glmark2 frame.
#[test]
fn presented_composite_matches_the_offscreen_atlas_it_samples() {
    let mut executor = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(executor) => executor,
        Err(_) => return,
    };
    let mut context = GlContext::new();
    context.set_surface(GlSurface {
        have: true,
        width: W,
        height: H,
    });
    context.set_present_frame(
        Some(SurfaceToken::new(7).unwrap()),
        Some(FrameSerial::new(1).unwrap()),
    );

    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    record::tex_parameter(&mut context, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    record::tex_parameter(&mut context, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    let fbo = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    record::viewport(&mut context, [0, 0, 16, 16]);
    let quadrants = program(&mut context, VS_FLAT, FS_QUADRANTS);
    fullscreen(&mut context, quadrants, false);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    record::viewport(&mut context, [0, 0, W as i32, H as i32]);
    let composite = program(&mut context, VS_TEXTURE, FS_TEXTURE);
    record::uniform_sampler(&mut context, 0, 0);
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    fullscreen(&mut context, composite, true);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let rendered = frame::Frame::build(&mut context).expect("multi-FBO frame");
    let target = rendered.present.1;
    let offscreen = rendered
        .cmds
        .iter()
        .find_map(|command| match command {
            hl_gpu::Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => {
                Some(*id)
            }
            _ => None,
        })
        .expect("offscreen target");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut session, &mut executor, 0, &rendered.cmds)
        .expect("execute GL-lowered frame");

    let atlas_bytes = executor
        .read_texture(&session.resources, offscreen)
        .expect("read raw offscreen target");
    let atlas_pixel = |x: usize, y: usize| -> [u8; 4] {
        let at = (y * 16 + x) * 4;
        atlas_bytes[at..at + 4].try_into().unwrap()
    };
    assert_eq!(atlas_pixel(2, 2), [0, 255, 0, 255]);
    assert_eq!(atlas_pixel(13, 2), [255, 255, 0, 255]);
    assert_eq!(atlas_pixel(2, 13), [255, 0, 0, 255]);
    assert_eq!(atlas_pixel(13, 13), [0, 0, 255, 255]);
    let bytes = executor
        .read_texture(&session.resources, target)
        .expect("read raw present target");
    // The rendered-FBO sampler conversion still applies (GL texel v=0 is the FBO's bottom row), so the
    // composite reproduces the atlas rather than mirroring it.
    assert_eq!(pixel(&bytes, 8, 8), atlas_pixel(2, 2), "top-left quadrant");
    assert_eq!(
        pixel(&bytes, 56, 8),
        atlas_pixel(13, 2),
        "top-right quadrant"
    );
    assert_eq!(
        pixel(&bytes, 8, 56),
        atlas_pixel(2, 13),
        "bottom-left quadrant"
    );
    assert_eq!(
        pixel(&bytes, 56, 56),
        atlas_pixel(13, 13),
        "bottom-right quadrant"
    );
    assert_eq!(pixel(&bytes, 8, 8), [0, 255, 0, 255], "top-left GREEN");
    assert_eq!(pixel(&bytes, 8, 56), [255, 0, 0, 255], "bottom-left RED");
}
