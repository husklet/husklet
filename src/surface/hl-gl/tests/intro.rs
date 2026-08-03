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
    c.set_surface(GlSurface {
        have: true,
        width: 800,
        height: 600,
    });
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

/// Block reflection round-trips through a program that actually DECLARES a block.
///
/// This test asserted the defect it sat next to. It drove a program whose only uniform is a plain
/// `uniform vec4 uTint` — no interface block at all — asked for a block called `Uniforms`, and asserted
/// it got a valid index, a data size and a name back. Every one of those was the synthetic default-block
/// entry answering, so the assertions passed while describing behaviour a conformant driver does not
/// have: the default uniform block has no index to look up. What the test was really defending is the
/// binding round-trip, which is real, so that moves onto a program with a declared block and the
/// default-block claim is asserted the other way round.
#[test]
fn uniform_block_index_is_stable_and_binding_round_trips() {
    let mut c = ctx_800x600();
    let prog = program_with_blocks(&mut c);

    // A declared block's index is its declaration position, and the same name returns the same index.
    let i0 = intro::uniform_block_index(&mut c, prog, "Matrices");
    assert_eq!(i0, 0);
    assert_eq!(intro::uniform_block_index(&mut c, prog, "Matrices"), i0);

    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, i0, GL_UNIFORM_BLOCK_ACTIVE_UNIFORMS),
        Some(1),
        "one member (uMvp) in the Matrices block"
    );
    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, i0, GL_UNIFORM_BLOCK_DATA_SIZE),
        Some(64),
        "a mat4 block is 64 bytes of std140"
    );

    // Default binding is the declared one; glUniformBlockBinding sets it, and it reads back.
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
        Some("Matrices")
    );

    // An out-of-range block index → GL_INVALID_VALUE via the shim path (None here).
    assert!(intro::active_uniform_blockiv(&mut c, prog, 99, GL_UNIFORM_BLOCK_BINDING).is_none());
    // A name the program does not declare as a block has no index — including the name of the synthetic
    // block this driver used to invent.
    assert_eq!(
        intro::uniform_block_index(&mut c, prog, "Uniforms"),
        GL_INVALID_INDEX
    );
    // An unknown program has no block namespace.
    assert_eq!(
        intro::uniform_block_index(&mut c, 4242, "Matrices"),
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
fn uniform_getters_convert_declared_scalar_types_instead_of_reinterpreting_bits() {
    const TYPED_FS: &str = "#version 300 es\nprecision mediump float;\nuniform bool uBool;\nuniform int uInt;\nuniform float uFloat;\nout vec4 color;\nvoid main(){ color = vec4(uBool ? 1.0 : 0.0, float(uInt), uFloat, 1.0); }\n";
    let mut c = ctx_800x600();
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, TYPED_FS);
    record::compile_shader(&mut c, fs);
    let program = record::create_program(&mut c);
    record::attach_shader(&mut c, program, vs);
    record::attach_shader(&mut c, program, fs);
    assert!(record::link_program(&mut c, program));
    record::use_program(&mut c, program);

    let bool_location = query::uniform_location(&c, program, "uBool");
    let int_location = query::uniform_location(&c, program, "uInt");
    let float_location = query::uniform_location(&c, program, "uFloat");
    record::set_uniform(
        &mut c,
        bool_location,
        record::UniformSetter::Float(1),
        1,
        &(-7.0_f32).to_le_bytes(),
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    record::set_uniform(
        &mut c,
        int_location,
        record::UniformSetter::Int(1),
        1,
        &(-3_i32).to_le_bytes(),
    );
    record::set_uniform(
        &mut c,
        float_location,
        record::UniformSetter::Float(1),
        1,
        &2.75_f32.to_le_bytes(),
    );

    assert_eq!(intro::get_uniform_f32(&c, program, bool_location), Some(vec![1.0]));
    assert_eq!(intro::get_uniform_i32(&c, program, bool_location), Some(vec![1]));
    assert_eq!(intro::get_uniform_f32(&c, program, int_location), Some(vec![-3.0]));
    assert_eq!(intro::get_uniform_i32(&c, program, float_location), Some(vec![2]));
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
    assert!(c.draws().is_empty());

    record::clear_buffer_color(&mut c, 0, [0.1, 0.2, 0.3, 0.4]);

    // Exactly one clear draw was recorded, carrying the requested color + a full-surface rect.
    assert_eq!(c.draws().len(), 1);
    let d = &c.draws()[0];
    assert!(d.is_clear);
    assert_eq!(d.clear, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(d.clear_rect, [0, 0, 800, 600]);
    assert_eq!(
        d.clear_draw_buffer,
        Some(0),
        "the clear is scoped to its attachment"
    );
    // ES 3.0 §4.2.3: the value travels with the call. Only `glClearColor` sets GL_COLOR_CLEAR_VALUE, so
    // a `glClearBufferfv` must leave it at the initial (0, 0, 0, 0) — this test asserted the opposite,
    // which let the recording clobber state the app had set for its own `glClear`s.
    let mut f = [0f32; 4];
    query::get_floatv(&c, GL_COLOR_CLEAR_VALUE, &mut f);
    assert_eq!(f, [0.0, 0.0, 0.0, 0.0]);
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
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_RED_SIZE),
        8
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_DEPTH_SIZE),
        0
    );
    record::renderbuffer_storage_multisample(
        &mut c,
        GL_RENDERBUFFER,
        1,
        GL_DEPTH24_STENCIL8,
        32,
        24,
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_DEPTH_SIZE),
        24
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_STENCIL_SIZE),
        8
    );
    assert_eq!(
        intro::renderbuffer_parameter(&c, GL_RENDERBUFFER, GL_RENDERBUFFER_SAMPLES),
        1
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

    // ES 3.0 §7.3: glDeleteProgram of the CURRENT program only flags it; it stays current and stays live
    // until glUseProgram moves away. Pinned in full by tests/gles_object_lifetime.rs.
    record::delete_program(&mut c, prog);
    assert!(c.programs.contains(prog));
    assert_eq!(c.current_program(), prog);
    record::use_program(&mut c, 0);
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

const VS_BLOCKS: &str = "#version 300 es\n\
layout(std140) uniform Matrices { mat4 uMvp; };\n\
layout(std140) uniform Material { vec4 uTint; };\n\
void main(){ gl_Position = uMvp * vec4(uTint.xy, 0.0, 1.0); }\n";
const FS_BLOCKS: &str =
    "#version 300 es\nprecision mediump float;\nout vec4 o;\nvoid main(){ o = vec4(1.0); }\n";

fn program_with_blocks(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS_BLOCKS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS_BLOCKS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    prog
}

/// The DEFAULT uniform block is not a named block, and modelling it as one shifted every real block by a
/// place.
///
/// Measured before the fix: a program declaring `Matrices` then `Material` reports them at indices 1 and 2
/// against a conformant driver's 0 and 1, index 0 is a synthetic block named `Uniforms`, and the two real
/// blocks report a data size and active-uniform count of zero while the synthetic one reports the
/// program's whole flattened uniform buffer. `GL_ACTIVE_UNIFORM_BLOCKS` is not implemented at all — the
/// enum was not even declared — so the count was zero, which put every index out of range for an
/// application that enumerates rather than looks up by name.
///
/// The cause is a category error rather than an off-by-one, and correcting a constant would leave it in
/// place. `seed_blocks` inserts a block named `Uniforms` at index 0 for any program with plain data
/// uniforms, to model the implicit block `glUniform*` writes into. ES 3.0 §2.12.6 does not make that a
/// named block: it has no index, it is excluded from `GL_ACTIVE_UNIFORM_BLOCKS`, and its members report
/// `GL_UNIFORM_BLOCK_INDEX` of -1 — which `uniformsiv` in this same file already returns correctly, so
/// the model contradicts itself two functions apart.
///
/// The table is now built at link from the blocks the shader declares, in declaration order, and the
/// synthetic entry is gone.
#[test]
fn declared_uniform_blocks_are_indexed_from_zero_in_declaration_order() {
    let mut c = ctx_800x600();
    let prog = program_with_blocks(&mut c);

    assert_eq!(
        intro::uniform_block_index(&mut c, prog, "Matrices"),
        0,
        "the first DECLARED block is index 0; the default block is not a named block"
    );
    assert_eq!(intro::uniform_block_index(&mut c, prog, "Material"), 1);
    assert_eq!(
        intro::active_uniform_block_name(&mut c, prog, 0).as_deref(),
        Some("Matrices")
    );
    // Each block reports its OWN std140 size, not the program's flattened uniform buffer: a mat4 block is
    // 64 bytes and a vec4 block is 16. Reporting zero is what makes an application lay its buffer out at
    // the driver's own offsets and corrupt it.
    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, 0, GL_UNIFORM_BLOCK_DATA_SIZE),
        Some(64)
    );
    assert_eq!(
        intro::active_uniform_blockiv(&mut c, prog, 1, GL_UNIFORM_BLOCK_DATA_SIZE),
        Some(16)
    );
    // And a program with only plain data uniforms has NO named blocks at all.
    let plain = linked_program(&mut c);
    assert_eq!(
        intro::uniform_block_index(&mut c, plain, "Uniforms"),
        GL_INVALID_INDEX,
        "the default uniform block has no index to look up"
    );
}

/// `GL_ACTIVE_UNIFORM_BLOCKS` is what an application enumerates blocks with, and it was not merely
/// answering zero — neither it nor `GL_ACTIVE_UNIFORM_BLOCK_MAX_NAME_LENGTH` was declared anywhere in the
/// driver, so both fell through the program query's default arm. That is the worst shape a query can have:
/// zero is exactly what a program with no blocks reports, so nothing outside could tell the difference,
/// and every valid index was out of range of the count that describes it.
///
/// The pairing with a program that genuinely declares none is the whole point of the test.
#[test]
fn the_active_block_count_describes_the_blocks_that_exist() {
    let mut c = ctx_800x600();
    let blocks = program_with_blocks(&mut c);
    assert_eq!(
        query::get_programiv(&c, blocks, GL_ACTIVE_UNIFORM_BLOCKS),
        2,
        "Matrices and Material"
    );
    assert_eq!(
        query::get_programiv(&c, blocks, GL_ACTIVE_UNIFORM_BLOCK_MAX_NAME_LENGTH),
        "Material".len() as i32 + 1,
        "the longest block name, with its terminator"
    );
    // Every index the count describes must resolve, which is the property that was broken.
    for index in 0..query::get_programiv(&c, blocks, GL_ACTIVE_UNIFORM_BLOCKS) as u32 {
        assert!(
            intro::active_uniform_block_name(&mut c, blocks, index).is_some(),
            "block {index} is inside the reported count and must resolve"
        );
    }

    // A program with only plain data uniforms declares no blocks — the default block is not one.
    let plain = linked_program(&mut c);
    assert_eq!(query::get_programiv(&c, plain, GL_ACTIVE_UNIFORM_BLOCKS), 0);
    assert_eq!(
        query::get_programiv(&c, plain, GL_ACTIVE_UNIFORM_BLOCK_MAX_NAME_LENGTH),
        0
    );
}
