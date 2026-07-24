//! Tests for the ES3 completeness pass's REAL-behavior services: uniform-block reflection
//! (`glGetUniformBlockIndex`/`glUniformBlockBinding`/`glGetActiveUniformBlockiv`), the `glProgramUniform*`
//! DSA writers (into a named program's uniform block), `glClearBufferfv` recording a scoped clear, the
//! program-resource introspection (`glGetProgramResource*`/`glGetProgramInterfaceiv`), the uniform-index /
//! active-uniform reflection (`glGetUniformIndices`/`glGetActiveUniformsiv`), and the smaller real getters.
//!
//! Driven directly against the `hl_gl` service layer (no socket, no GPU, no guest cdylib) — the same pure
//! state-inspection surface the shim's C-ABI entry points marshal.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{intro, query, record};

const VS: &str = "attribute vec2 aPos;\nattribute vec3 aColor;\nvarying vec3 vColor;\nvoid main(){ vColor = aColor; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
// One data uniform (uTint: vec4) + one sampler uniform (uTex: sampler2D), and a named fragment output.
const FS: &str = "#version 300 es\nprecision mediump float;\nin vec3 vColor;\nuniform vec4 uTint;\nuniform sampler2D uTex;\nout vec4 fragColor;\nvoid main(){ fragColor = texture(uTex, vColor.xy) * uTint; }\n";

fn ctx_800x600() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: 800,
        height: 600,
    };
    c
}

fn linked_program(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

// ---- uniform-block reflection --------------------------------------------------------------------

#[test]
fn uniform_block_index_is_stable_and_binding_round_trips() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // A first lookup of a name assigns a stable index; the same name returns the same index.
    let i0 = intro::uniform_block_index(&mut c, prog, "Uniforms");
    assert_ne!(i0, GL_INVALID_INDEX);
    assert_eq!(intro::uniform_block_index(&mut c, prog, "Uniforms"), i0);

    // Block 0 reflects the program's real implicit block: its data size + active-uniform count.
    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, i0, GL_UNIFORM_BLOCK_ACTIVE_UNIFORMS),
        Some(1),
        "one data uniform (uTint) in the reflected block"
    );
    let data_size =
        intro::active_uniform_blockiv(&mut c, prog, i0, GL_UNIFORM_BLOCK_DATA_SIZE).unwrap();
    assert!(
        data_size >= 16,
        "a vec4 block is at least 16 bytes, got {data_size}"
    );

    // Default binding is 0; glUniformBlockBinding sets it, and it reads back.
    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, i0, GL_UNIFORM_BLOCK_BINDING),
        Some(0)
    );
    intro::uniform_block_binding(&mut c, prog, i0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, i0, GL_UNIFORM_BLOCK_BINDING),
        Some(3)
    );

    // The block name round-trips.
    assert_eq!(
        intro::active_uniform_block_name(&mut c, prog, i0).as_deref(),
        Some("Uniforms")
    );

    // An out-of-range block index → GL_INVALID_VALUE via the shim path (None here).
    assert!(intro::active_uniform_blockiv(&mut c, prog, 99, GL_UNIFORM_BLOCK_BINDING).is_none());
    // An unknown program has no block namespace.
    assert_eq!(
        intro::uniform_block_index(&mut c, 4242, "Uniforms"),
        GL_INVALID_INDEX
    );
}

#[test]
fn uniform_block_binding_rejects_bad_program_and_binding() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // An unknown program → GL_INVALID_VALUE.
    intro::uniform_block_binding(&mut c, 4242, 0, 0);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);

    // A binding beyond the cap → GL_INVALID_VALUE.
    intro::uniform_block_binding(&mut c, prog, 0, MAX_UNIFORM_BUFFER_BINDINGS + 1);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

// ---- glGetUniformIndices / glGetActiveUniformsiv -------------------------------------------------

#[test]
fn uniform_indices_and_active_uniformsiv_reflect_the_tables() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // The data uniform is index 0, the sampler is index 1 (data-uniforms-first enumeration).
    assert_eq!(intro::uniform_index(&c, prog, "uTint"), 0);
    assert_eq!(intro::uniform_index(&c, prog, "uTex"), 1);
    assert_eq!(intro::uniform_index(&c, prog, "nope"), GL_INVALID_INDEX);

    // The data uniform reflects vec4 / size 1 / block 0 / a real byte offset.
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 0, GL_UNIFORM_TYPE),
        Some(GL_FLOAT_VEC4 as i32)
    );
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 0, GL_UNIFORM_SIZE),
        Some(1)
    );
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 0, GL_UNIFORM_BLOCK_INDEX),
        Some(0)
    );
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 0, GL_UNIFORM_OFFSET),
        Some(0)
    );
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 0, GL_UNIFORM_NAME_LENGTH),
        Some("uTint".len() as i32 + 1)
    );

    // The sampler uniform is not backed by a buffer block (offset / block index -1).
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 1, GL_UNIFORM_TYPE),
        Some(GL_SAMPLER_2D as i32)
    );
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 1, GL_UNIFORM_BLOCK_INDEX),
        Some(-1)
    );
    assert_eq!(
        intro::active_uniformsiv(&c, prog, 1, GL_UNIFORM_OFFSET),
        Some(-1)
    );

    // Out of range → None (the shim writes nothing).
    assert!(intro::active_uniformsiv(&c, prog, 2, GL_UNIFORM_TYPE).is_none());
}

// ---- glProgramUniform* (DSA) ---------------------------------------------------------------------

#[test]
fn program_uniform_writes_into_the_named_program_block() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);
    // Do NOT glUseProgram anything else — glProgramUniform must target `prog` regardless of the binding.
    record::use_program(&mut c, 0);

    // Resolve uTint's location (declaration index) and write a vec4 through the DSA setter.
    let loc = query::uniform_location(&c, prog, "uTint");
    assert!(loc >= 0);
    let want = [0.25f32, 0.5, 0.75, 1.0];
    let mut bytes = Vec::new();
    for v in want {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    record::program_uniform_at(&mut c, prog, loc, &bytes);

    // Read the value straight back out of the program's uniform block.
    let got = intro::get_uniform_bytes(&c, prog, loc).expect("uTint bytes");
    assert_eq!(got.len(), 16, "vec4 is 16 bytes");
    let got_f: Vec<f32> = got
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    assert_eq!(got_f, want);
}

#[test]
fn program_uniform_sampler_binds_texture_unit_on_the_named_program() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);
    let loc = query::uniform_location(&c, prog, "uTex");
    assert!(loc >= 0);
    // glProgramUniform1i on a sampler maps it to a texture unit (declaration-index convention).
    record::program_uniform_sampler(&mut c, prog, loc as usize, 5);
    assert_eq!(intro::get_sampler_unit(&c, prog, loc), Some(5));
}

// ---- glClearBufferfv -----------------------------------------------------------------------------

#[test]
fn clear_buffer_color_records_a_scoped_clear() {
    let mut c = ctx_800x600();
    assert!(c.draws.is_empty());

    record::clear_buffer_color(&mut c, [0.1, 0.2, 0.3, 0.4]);

    // Exactly one clear draw was recorded, carrying the requested color + a full-surface rect.
    assert_eq!(c.draws.len(), 1);
    let d = &c.draws[0];
    assert!(d.is_clear);
    assert_eq!(d.clear, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(d.clear_rect, [0, 0, 800, 600]);
    // The clear color also updated the live state (glGetFloatv round-trip).
    let mut f = [0f32; 4];
    query::get_floatv(&c, GL_COLOR_CLEAR_VALUE, &mut f);
    assert_eq!(f, [0.1, 0.2, 0.3, 0.4]);
}

// ---- glGetProgramResource* / glGetProgramInterfaceiv ---------------------------------------------

#[test]
fn program_resource_introspection_reflects_uniforms_inputs_outputs() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);

    // GL_UNIFORM: two active resources (uTint data + uTex sampler).
    assert_eq!(
        intro::program_interfaceiv(&c, prog, GL_UNIFORM, GL_ACTIVE_RESOURCES),
        Some(2)
    );
    assert_eq!(
        intro::program_resource_index(&c, prog, GL_UNIFORM, "uTint"),
        0
    );
    assert_eq!(
        intro::program_resource_index(&c, prog, GL_UNIFORM, "uTex"),
        1
    );
    assert_eq!(
        intro::program_resource_index(&c, prog, GL_UNIFORM, "nope"),
        GL_INVALID_INDEX
    );
    assert_eq!(
        intro::program_resource_name(&c, prog, GL_UNIFORM, 0).as_deref(),
        Some("uTint")
    );
    assert_eq!(
        intro::program_resourceiv(&c, prog, GL_UNIFORM, 0, GL_TYPE),
        Some(GL_FLOAT_VEC4 as i32)
    );
    assert_eq!(
        intro::program_resourceiv(&c, prog, GL_UNIFORM, 0, GL_NAME_LENGTH),
        Some("uTint".len() as i32 + 1)
    );

    // GL_PROGRAM_INPUT: two vertex attributes in declaration order (aPos, aColor).
    assert_eq!(
        intro::program_interfaceiv(&c, prog, GL_PROGRAM_INPUT, GL_ACTIVE_RESOURCES),
        Some(2)
    );
    assert_eq!(
        intro::program_resource_location(&c, prog, GL_PROGRAM_INPUT, "aPos"),
        0
    );
    assert_eq!(
        intro::program_resource_location(&c, prog, GL_PROGRAM_INPUT, "aColor"),
        1
    );
    assert_eq!(
        intro::program_resourceiv(&c, prog, GL_PROGRAM_INPUT, 1, GL_TYPE),
        Some(GL_FLOAT_VEC3 as i32)
    );

    // GL_PROGRAM_OUTPUT: the one named fragment output resolves to location 0.
    assert_eq!(
        intro::program_interfaceiv(&c, prog, GL_PROGRAM_OUTPUT, GL_ACTIVE_RESOURCES),
        Some(1)
    );
    assert_eq!(intro::frag_data_location(&c, prog, "fragColor"), 0);
    assert_eq!(intro::frag_data_location(&c, prog, "missing"), -1);
}

// ---- smaller real getters ------------------------------------------------------------------------

#[test]
fn is_enabled_and_shader_source_reflect_state() {
    let mut c = ctx_800x600();
    assert!(!c.is_enabled(GL_BLEND));
    record::enable(&mut c, GL_BLEND);
    assert!(c.is_enabled(GL_BLEND));
    assert!(!c.is_enabled(GL_DEPTH_TEST));
    // An unmodeled cap is honestly false.
    assert!(!c.is_enabled(0xBEEF));

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    assert_eq!(c.get_shader_source(vs), VS);
    // An unknown shader has empty source.
    assert_eq!(c.get_shader_source(9999), "");
}

#[test]
fn renderbuffer_and_tex_level_parameters_report_real_extents() {
    let mut c = ctx_800x600();

    // Renderbuffer storage extent round-trips through glGetRenderbufferParameteriv.
    let rbo = record::gen_renderbuffer(&mut c);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rbo);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA8, 64, 48);
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH),
        64
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_HEIGHT),
        48
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_INTERNAL_FORMAT),
        GL_RGBA8 as i32
    );

    // A bound texture's level-0 extent round-trips through glGetTexLevelParameteriv.
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    let rgba = vec![0u8; 32 * 16 * 4];
    record::tex_image_2d(&mut c, 32, 16, &rgba);
    assert_eq!(
        intro::tex_level_parameter(&c, GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH),
        32
    );
    assert_eq!(
        intro::tex_level_parameter(&c, GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT),
        16
    );
}

// ---- program / shader lifecycle ------------------------------------------------------------------

#[test]
fn delete_and_detach_mutate_program_state() {
    let mut c = ctx_800x600();
    let prog = linked_program(&mut c);
    assert!(c.programs.contains(prog));

    // glDeleteProgram of the current program removes it and clears the binding.
    record::delete_program(&mut c, prog);
    assert!(!c.programs.contains(prog));
    assert_eq!(c.current_program(), 0);

    // glDetachShader errors: unknown program → GL_INVALID_VALUE.
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let p2 = record::create_program(&mut c);
    record::attach_shader(&mut c, p2, vs);
    record::detach_shader(&mut c, 4242, vs);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // Detaching an unattached shader → GL_INVALID_OPERATION.
    let other = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::detach_shader(&mut c, p2, other);
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
    // A real detach clears the slot.
    record::detach_shader(&mut c, p2, vs);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}
