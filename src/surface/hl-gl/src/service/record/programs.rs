use super::*;
use crate::model::glconst::MAX_TEXTURE_UNITS;

// ---- shaders + programs --------------------------------------------------------------------------

/// `glCreateShader(kind)`.
impl GlContext {
    pub fn create_shader(&mut self, kind: u32) -> u32 {
        if !matches!(
            kind,
            GL_VERTEX_SHADER | GL_FRAGMENT_SHADER | GL_COMPUTE_SHADER
        ) {
            self.set_gl_error(GL_INVALID_ENUM);
            return 0;
        }
        self.programs.create_shader(kind)
    }
}

#[cfg(test)]
mod buffer_snapshot_tests {
    use super::*;
    use std::sync::Arc;

    fn context_with_buffer() -> (GlContext, u32) {
        let mut context = GlContext::new();
        let name = context.buffers.gen();
        context
            .buffers
            .set_data(name, GL_ARRAY_BUFFER, &[1, 2, 3, 4], 0);
        context.local.array_buffer = name;
        context.local.attr[0].enabled = true;
        context.local.attr[0].buffer = name;
        (context, name)
    }

    #[test]
    fn repeated_draw_snapshots_share_unchanged_buffer_storage() {
        let (mut context, _) = context_with_buffer();

        crate::service::record::draw_arrays(&mut context, GL_TRIANGLES, 0, 1);
        crate::service::record::draw_arrays(&mut context, GL_TRIANGLES, 0, 1);

        assert!(Arc::ptr_eq(
            &context.local.recording.draws[0].buffers[0].data,
            &context.local.recording.draws[1].buffers[0].data
        ));
    }

    #[test]
    fn subdata_mutation_detaches_from_earlier_draw_snapshot() {
        let (mut context, name) = context_with_buffer();
        let before = context.snapshot(true);

        context.buffers.set_sub_data(name, 1, &[9, 8]);
        let after = context.snapshot(true);

        assert_eq!(before.buffers[0].data.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(after.buffers[0].data.as_slice(), &[1, 9, 8, 4]);
        assert!(!Arc::ptr_eq(
            &before.buffers[0].data,
            &after.buffers[0].data
        ));
    }

    #[test]
    fn mapped_mutation_detaches_from_earlier_draw_snapshot() {
        let (mut context, name) = context_with_buffer();
        let before = context.snapshot(true);

        assert_eq!(
            context.buffers.map_range(name, 1, 2, GL_MAP_WRITE_BIT),
            Some(1)
        );
        let pointer = context.buffers.mapped_ptr(name, 1).unwrap();
        // SAFETY: the pointer addresses the live mapped two-byte range and is used before unmap.
        unsafe {
            pointer.write(7);
            pointer.add(1).write(6);
        }
        context.buffers.take_map(name).unwrap();
        let after = context.snapshot(true);

        assert_eq!(before.buffers[0].data.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(after.buffers[0].data.as_slice(), &[1, 7, 6, 4]);
        assert!(!Arc::ptr_eq(
            &before.buffers[0].data,
            &after.buffers[0].data
        ));
    }

    #[test]
    fn mapped_buffer_draw_is_rejected_before_snapshot_and_pointer_stays_exclusive() {
        let (mut context, name) = context_with_buffer();
        assert_eq!(
            context.buffers.map_range(name, 1, 2, GL_MAP_WRITE_BIT),
            Some(1)
        );
        let pointer = context.buffers.mapped_ptr(name, 1).unwrap();

        crate::service::record::draw_arrays(&mut context, GL_TRIANGLES, 0, 1);

        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        assert!(context.local.recording.draws.is_empty());
        // SAFETY: the rejected draw did not alter the live mapped allocation.
        unsafe {
            pointer.write(5);
            pointer.add(1).write(6);
        }
        context.buffers.take_map(name).unwrap();
        assert_eq!(
            context.buffers.get(name).unwrap().data.as_slice(),
            &[1, 5, 6, 4]
        );
    }

    #[test]
    fn clear_does_not_snapshot_a_mapped_vertex_buffer() {
        let (mut context, name) = context_with_buffer();
        assert_eq!(
            context.buffers.map_range(name, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(name, 0).unwrap();

        context.record_clear();

        assert_eq!(context.take_gl_error(), GL_NO_ERROR);
        assert_eq!(context.local.recording.draws.len(), 1);
        assert!(context.local.recording.draws[0].is_clear);
        assert!(context.local.recording.draws[0].buffers.is_empty());
        // SAFETY: recording a clear did not snapshot or replace the mapped allocation.
        unsafe { pointer.write(7) };
    }

    #[test]
    fn mapped_separate_vertex_binding_rejects_draw() {
        let (mut context, name) = context_with_buffer();
        context.local.attr[0].buffer = 0;
        context.local.attr[0].binding = Some(0);
        context.local.vertex_bindings[0].buffer = name;
        assert_eq!(
            context.buffers.map_range(name, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(name, 0).unwrap();

        crate::service::record::draw_arrays(&mut context, GL_TRIANGLES, 0, 1);

        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        assert!(context.local.recording.draws.is_empty());
        // SAFETY: the rejected draw left the mapped allocation unchanged.
        unsafe { pointer.write(7) };
    }

    #[test]
    fn buffer_data_cannot_replace_live_mapped_storage() {
        let (mut context, name) = context_with_buffer();
        assert_eq!(
            context.buffers.map_range(name, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(name, 0).unwrap();

        crate::service::record::buffer_data(&mut context, GL_ARRAY_BUFFER, &[9, 9], 0);

        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        // SAFETY: rejected glBufferData preserved the live mapped allocation.
        unsafe { pointer.write(7) };
        assert_eq!(
            context.buffers.get(name).unwrap().data.as_slice(),
            &[7, 2, 3, 4]
        );
    }

    #[test]
    fn buffer_sub_data_cannot_mutate_live_mapped_storage() {
        let (mut context, name) = context_with_buffer();
        assert_eq!(
            context.buffers.map_range(name, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(name, 0).unwrap();

        crate::service::record::buffer_sub_data(&mut context, GL_ARRAY_BUFFER, 1, &[9, 9]);

        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        // SAFETY: rejected glBufferSubData preserved the live mapped allocation.
        unsafe { pointer.write(7) };
        assert_eq!(
            context.buffers.get(name).unwrap().data.as_slice(),
            &[7, 2, 3, 4]
        );
    }

    #[test]
    fn copy_buffer_sub_data_rejects_a_mapped_source_or_destination() {
        let (mut context, source) = context_with_buffer();
        let destination = context.buffers.gen();
        context
            .buffers
            .set_data(destination, GL_COPY_WRITE_BUFFER, &[0, 0, 0, 0], 0);
        crate::service::record::bind_buffer(&mut context, GL_COPY_READ_BUFFER, source);
        crate::service::record::bind_buffer(&mut context, GL_COPY_WRITE_BUFFER, destination);
        assert_eq!(
            context.buffers.map_range(source, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(source, 0).unwrap();

        crate::service::record::copy_buffer_sub_data(
            &mut context,
            GL_COPY_READ_BUFFER,
            GL_COPY_WRITE_BUFFER,
            0,
            0,
            4,
        );

        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        // SAFETY: rejected copy preserved the live mapped source allocation.
        unsafe { pointer.write(7) };
        assert_eq!(
            context.buffers.get(destination).unwrap().data.as_slice(),
            &[0, 0, 0, 0]
        );

        context.buffers.take_map(source).unwrap();
        assert_eq!(
            context
                .buffers
                .map_range(destination, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let destination_pointer = context.buffers.mapped_ptr(destination, 0).unwrap();
        crate::service::record::copy_buffer_sub_data(
            &mut context,
            GL_COPY_READ_BUFFER,
            GL_COPY_WRITE_BUFFER,
            0,
            0,
            4,
        );
        assert_eq!(context.take_gl_error(), GL_INVALID_OPERATION);
        // SAFETY: rejected copy preserved the live mapped destination allocation.
        unsafe { destination_pointer.write(8) };
        assert_eq!(
            context.buffers.get(destination).unwrap().data.as_slice(),
            &[8, 0, 0, 0]
        );
    }

    #[test]
    fn deleting_a_mapped_buffer_retires_its_live_allocation() {
        let (mut context, name) = context_with_buffer();
        assert_eq!(
            context.buffers.map_range(name, 0, 4, GL_MAP_WRITE_BIT),
            Some(0)
        );
        let pointer = context.buffers.mapped_ptr(name, 0).unwrap();

        assert!(context.delete_buffer(name));

        assert_eq!(context.take_gl_error(), GL_NO_ERROR);
        assert!(context.buffers.get(name).is_none());
        assert_eq!(context.buffers.retired_mapping_count(), 1);
        // SAFETY: GL invalidates the application pointer at delete, but the model deliberately retains its
        // backing allocation until context teardown so an escaped FFI pointer can never dangle into Rust.
        unsafe { pointer.write(7) };
    }
}

/// `glShaderSource(shader, src)`.
pub fn shader_source(ctx: &mut GlContext, shader: u32, src: &str) {
    if !ctx.programs.has_shader(shader) {
        ctx.set_gl_error(if ctx.programs.contains(shader) {
            GL_INVALID_OPERATION
        } else {
            GL_INVALID_VALUE
        });
        return;
    }
    ctx.programs.shader_source(shader, src);
}

/// `glCompileShader(shader)`.
impl GlContext {
    pub fn compile_shader(&mut self, shader: u32) {
        // GLSL-ES §3.3: a shader may use only the constructs its declared `#version` defines. A 3.10
        // built-in under `#version 300 es` compiled here and failed on real hardware — the author's first
        // notice being a bug report from a device they do not have. Refuse it, and say which one.
        if let Some(source) = self
            .programs
            .shader(shader)
            .and_then(|sh| sh.src.as_deref())
        {
            let kind = self
                .programs
                .shader(shader)
                .map(|shader| shader.kind)
                .unwrap_or(0);
            if let Some(reason) = crate::adapter::glsl::invalid_declaration_identifier(source) {
                self.programs.fail_compile(shader, reason);
                return;
            }
            if let Some(reason) = crate::adapter::glsl::invalid_storage_declaration(source, kind) {
                self.programs.fail_compile(shader, reason);
                return;
            }
            if let Some(operator) = crate::adapter::glsl::reserved_operator(source) {
                self.programs.fail_compile(
                    shader,
                    format!("'{operator}' : reserved operator is not available in GLSL ES 1.00"),
                );
                return;
            }
            if let Some(reason) = crate::adapter::glsl::invalid_implicit_arithmetic(source) {
                self.programs.fail_compile(shader, reason);
                return;
            }
            if let Some(builtin) = crate::adapter::glsl::builtin_above_declared_version(source) {
                let version = crate::adapter::glsl::declared_es_version(source);
                self.programs.fail_compile(
                    shader,
                    format!(
                        "'{builtin}' : no matching overloaded function found — it was introduced in                          GLSL ES 3.10 and this shader declares #version {version} es"
                    ),
                );
                return;
            }
        }
        if !self.programs.has_shader(shader) {
            // ES 3.0 §2.11.1: a name that is not a shader object is GL_INVALID_VALUE, and one that names a
            // program is GL_INVALID_OPERATION. Both were silent no-ops, so a call on a DELETED shader
            // looked like it had worked.
            self.set_gl_error(if self.programs.contains(shader) {
                GL_INVALID_OPERATION
            } else {
                GL_INVALID_VALUE
            });
            return;
        }
        self.programs.compile_shader(shader);
    }
}

/// `glCreateProgram()`.
impl GlContext {
    pub fn create_program(&mut self) -> u32 {
        self.programs.create()
    }
}

/// `glAttachShader(program, shader)`.
pub fn attach_shader(ctx: &mut GlContext, program: u32, shader: u32) {
    if !ctx.programs.contains(program) {
        ctx.set_gl_error(if ctx.programs.has_shader(program) {
            GL_INVALID_OPERATION
        } else {
            GL_INVALID_VALUE
        });
        return;
    }
    if !ctx.programs.has_shader(shader) {
        ctx.set_gl_error(if ctx.programs.contains(shader) {
            GL_INVALID_OPERATION
        } else {
            GL_INVALID_VALUE
        });
        return;
    }
    if !ctx.programs.attach(program, shader) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    }
}

/// `glBindAttribLocation(program, index, name)` — set the location used by the next program link.
pub fn bind_attrib(ctx: &mut GlContext, program: u32, index: u32, name: &str) {
    if index as usize >= crate::model::program::MAX_ATTR {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if name.starts_with("gl_") {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if !ctx.programs.contains(program) {
        ctx.set_gl_error(if ctx.programs.has_shader(program) {
            GL_INVALID_OPERATION
        } else {
            GL_INVALID_VALUE
        });
        return;
    }
    ctx.programs.bind_attrib(program, index, name);
}

/// `glLinkProgram(program)` — translate the attached GLSL-ES pair to shader-IR + reflect the layout.
impl GlContext {
    pub fn link_program(&mut self, program: u32) -> bool {
        if !self.programs.contains(program) {
            self.set_gl_error(if self.programs.has_shader(program) {
                GL_INVALID_OPERATION
            } else {
                GL_INVALID_VALUE
            });
            return false;
        }
        let linked = self.programs.link(program);
        if linked {
            self.reflect_uniform_blocks(program);
        }
        linked
    }

    /// Rebuild the program's uniform-block table from the blocks its shaders declare, in declaration
    /// order — which is the order and the identity `glGetUniformBlockIndex`,
    /// `glGetActiveUniformBlockName` and `glGetActiveUniformBlockiv` all answer for.
    ///
    /// Link is the right moment and the only honest one: the block set is a property of the linked
    /// program, and building it lazily on first lookup is what let a name the program never declared
    /// receive a valid index. A relink replaces the table, so a program relinked with different sources
    /// does not keep blocks it no longer has.
    ///
    /// The BINDING is deliberately re-seeded from the shader's own `layout(binding = N)` here rather than
    /// carried across a relink: GL resets a block's binding to its declared value when the program is
    /// linked, and an application that assigned one with `glUniformBlockBinding` is expected to assign it
    /// again.
    fn reflect_uniform_blocks(&mut self, program: u32) {
        let Some(prog) = self.programs.program(program) else {
            return;
        };
        let declared = crate::adapter::glsl::StageSources::new(&prog.vs_src, &prog.fs_src)
            .declared_uniform_blocks();
        let blocks = declared
            .into_iter()
            .map(|block| crate::model::context::UniformBlock {
                name: block.name.clone(),
                binding: block.binding,
                data_size: block.std140_size(),
                members: block.members.len() as i32,
            })
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            self.uniform_blocks.remove(&program);
        } else {
            self.uniform_blocks.insert(program, blocks);
        }
    }

    /// `glUseProgram(program)`.
    pub fn use_program(&mut self, program: u32) {
        if program != 0 && (self.program_is_deleted(program) || !self.programs.contains(program)) {
            self.set_gl_error(if self.programs.has_shader(program) {
                GL_INVALID_OPERATION
            } else {
                GL_INVALID_VALUE
            });
            return;
        }
        let outgoing = self.local.cur_prog;
        self.local.cur_prog = program;
        // ES 3.0 §7.3: the program we just stopped using is no longer part of the current rendering state,
        // so a deletion flagged while it was current takes effect now.
        if outgoing != program && self.programs.flagged(outgoing) {
            self.destroy_program(outgoing);
        }
    }
}

pub const CREATE_SHADER: fn(&mut GlContext, u32) -> u32 = GlContext::create_shader;
pub const COMPILE_SHADER: fn(&mut GlContext, u32) = GlContext::compile_shader;
pub const CREATE_PROGRAM: fn(&mut GlContext) -> u32 = GlContext::create_program;
pub const LINK_PROGRAM: fn(&mut GlContext, u32) -> bool = GlContext::link_program;
pub const USE_PROGRAM: fn(&mut GlContext, u32) = GlContext::use_program;
pub use COMPILE_SHADER as compile_shader;
pub use CREATE_PROGRAM as create_program;
pub use CREATE_SHADER as create_shader;
pub use LINK_PROGRAM as link_program;
pub use USE_PROGRAM as use_program;

/// `glUniform1i(samplerLocation, unit)` — map a sampler uniform (by declaration index) to a texture
/// unit. Simplified: `sampler_index` is the sampler's position in the program's `samp_names`.
pub fn uniform_sampler(ctx: &mut GlContext, sampler_index: usize, unit: i32) {
    if let Some(p) = ctx.programs.get_mut(ctx.local.cur_prog) {
        if sampler_index < p.samp_units.len() {
            p.samp_units[sampler_index] = unit;
        }
    }
}

/// `glUniform*` for a data uniform — write `bytes` into the bound program's uniform-block buffer at the
/// named member's offset. Simplified name-keyed write (real GL uses integer locations).
pub fn uniform_data(ctx: &mut GlContext, name: &str, bytes: &[u8]) {
    if let Some(p) = ctx.programs.get_mut(ctx.local.cur_prog) {
        if let Some(u) = p.unis.iter().find(|u| u.name == name) {
            u.write(&mut p.ubuf, bytes);
        }
    }
}

/// `glUniform*`/`glUniformMatrix*` — write the already-marshalled little-endian `bytes` of a data uniform
/// into the bound program's uniform-block buffer. `location` is the uniform's declaration index (its
/// position in the program's reflected `unis`), matching the sampler-location convention used by
/// [`uniform_sampler`]; the frame builder ships the resulting `ubuf` at binding 1 so the draw's shader
/// reads the value. Out-of-range writes (bad location / oversized payload) are truncated to the slot.
pub fn uniform_at(ctx: &mut GlContext, location: usize, bytes: &[u8]) {
    let resolved = ctx
        .programs
        .program(ctx.local.cur_prog)
        .and_then(|program| program.location(location as i32));
    // ES 3.0 §2.11.7: a location that does not name a uniform of the CURRENT program (including "there is
    // no current program") is GL_INVALID_OPERATION. Location `-1` is the one exception and is filtered by
    // the caller before it gets here, so an unresolvable location at this point really is bogus. Accepting
    // it silently let an application write into nothing and read GL_NO_ERROR back.
    let Some(crate::model::program::UniformLocation::Data {
        declaration,
        element,
    }) = resolved
    else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    };
    if let Some(p) = ctx.programs.get_mut(ctx.local.cur_prog) {
        let Some(uniform) = p.unis.get(declaration) else {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        };
        uniform.write_from(&mut p.ubuf, element, bytes);
    }
}

/// The type and width encoded by one `glUniform*` entry point. Keeping this at the service boundary makes
/// every shim entry point use the same linked-program reflection and error rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UniformSetter {
    Float(u8),
    Int(u8),
    Matrix(u8),
}

/// Validate and apply an ES2 `glUniform*` update to the current program.
pub fn set_uniform(
    ctx: &mut GlContext,
    location: i32,
    setter: UniformSetter,
    count: i32,
    bytes: &[u8],
) {
    if count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let Some(program) = ctx.programs.program(ctx.local.cur_prog) else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    };
    if location == -1 {
        return;
    }
    if location < -1 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    let Some(resolved) = program.location(location) else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    };

    match resolved {
        crate::model::program::UniformLocation::Data {
            declaration,
            element,
        } => {
            let uniform = &program.unis[declaration];
            let compatible = match setter {
                UniformSetter::Float(width) => {
                    vector_width(&uniform.ty, "float", "vec") == Some(width)
                }
                UniformSetter::Int(width) => {
                    vector_width(&uniform.ty, "int", "ivec") == Some(width)
                        || vector_width(&uniform.ty, "bool", "bvec") == Some(width)
                }
                UniformSetter::Matrix(width) => matrix_width(&uniform.ty) == Some(width),
            };
            let elements = uniform.arr.max(1) as usize;
            if !compatible || count > 1 && uniform.arr == 0 || element + count as usize > elements {
                ctx.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
            uniform_at(ctx, location as usize, bytes);
        }
        crate::model::program::UniformLocation::Sampler { element } => {
            if setter != UniformSetter::Int(1) {
                ctx.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
            let values = bytes
                .chunks_exact(4)
                .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .collect::<Vec<_>>();
            if element + values.len() > program.samp_units.len() {
                ctx.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
            uniform_i32_at(ctx, location, &values);
        }
    }
}

fn vector_width(ty: &str, scalar: &str, vector: &str) -> Option<u8> {
    if ty == scalar {
        Some(1)
    } else {
        ty.strip_prefix(vector)?.parse().ok()
    }
}

fn matrix_width(ty: &str) -> Option<u8> {
    let rest = ty.strip_prefix("mat")?;
    if rest.len() == 1 {
        rest.parse().ok()
    } else {
        None
    }
}

/// `glUniform1i[v]` dispatches by the linked uniform's type: sampler locations update texture units while
/// integer/bool data locations update std140 bytes.
pub fn uniform_i32_at(ctx: &mut GlContext, location: i32, values: &[i32]) {
    let resolved = ctx
        .programs
        .program(ctx.local.cur_prog)
        .and_then(|program| program.location(location));
    match resolved {
        Some(crate::model::program::UniformLocation::Sampler { element }) => {
            if let Some(program) = ctx.programs.get_mut(ctx.local.cur_prog) {
                for (unit, value) in program.samp_units[element..].iter_mut().zip(values) {
                    *unit = *value;
                }
            }
        }
        Some(crate::model::program::UniformLocation::Data { .. }) => {
            let bytes = values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>();
            uniform_at(ctx, location as usize, &bytes);
        }
        None => {}
    }
}

// ---- program-uniform DSA setters (glProgramUniform*) ---------------------------------------------

/// `glProgramUniform*` for a data uniform — write `bytes` into `program`'s uniform-block buffer at the
/// member at declaration index `location` (the DSA form of [`uniform_at`], targeting a named program
/// rather than the bound one). Out-of-range writes are truncated to the member's slot.
pub fn program_uniform_at(ctx: &mut GlContext, program: u32, location: i32, bytes: &[u8]) {
    if location < 0 {
        return;
    }
    let resolved = ctx
        .programs
        .program(program)
        .and_then(|program| program.location(location));
    if let Some(p) = ctx.programs.get_mut(program) {
        let Some(crate::model::program::UniformLocation::Data {
            declaration,
            element,
        }) = resolved
        else {
            return;
        };
        let Some(uniform) = p.unis.get(declaration) else {
            return;
        };
        uniform.write_from(&mut p.ubuf, element, bytes);
    }
}

pub fn program_uniform_i32_at(ctx: &mut GlContext, program: u32, location: i32, values: &[i32]) {
    let resolved = ctx
        .programs
        .program(program)
        .and_then(|program| program.location(location));
    match resolved {
        Some(crate::model::program::UniformLocation::Sampler { element }) => {
            if let Some(program) = ctx.programs.get_mut(program) {
                for (unit, value) in program.samp_units[element..].iter_mut().zip(values) {
                    *unit = *value;
                }
            }
        }
        Some(crate::model::program::UniformLocation::Data { .. }) => {
            let bytes = values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>();
            program_uniform_at(ctx, program, location, &bytes);
        }
        None => {}
    }
}

/// `glProgramUniform1i(program, samplerLocation, unit)` — map the sampler at the program's public,
/// collision-free uniform `location` to a texture unit.
pub fn program_uniform_sampler(ctx: &mut GlContext, program: u32, location: usize, unit: i32) {
    let Ok(location) = i32::try_from(location) else {
        return;
    };
    program_uniform_i32_at(ctx, program, location, &[unit]);
}

// ---- program / shader lifecycle (glDeleteProgram / glDeleteShader / glDetachShader) ---------------

/// `glDeleteProgram` / `glDeleteShader` — the two DIFFERENT deletion rules GLES defines for these objects.
impl GlContext {
    /// `glDeleteProgram(program)` — ES 3.0 §7.3: "If a program object is in use as the current program for
    /// the current rendering context, it will be flagged for deletion, but it will not be deleted until it
    /// is no longer part of the current rendering state." So the current program keeps its binding and keeps
    /// working; `glUseProgram` moving away from it is what destroys it. An unreferenced program goes at once.
    ///
    /// The release-then-draw idiom depends on this: Skia, `QOpenGLShaderProgram`'s destructor and glmark2's
    /// scene teardown all `glDeleteProgram` a program that is still current and then keep drawing with it.
    pub fn delete_program(&mut self, program: u32) {
        if !self.programs.flag(program) {
            return;
        }
        if self.local.cur_prog != program {
            self.destroy_program(program);
        }
    }

    /// Remove a program object and retire its host resources. Used once the program stops being current.
    pub(crate) fn destroy_program(&mut self, program: u32) {
        if self.programs.delete(program) {
            // Retire the program's resident IR shader modules + render pipelines (queued Destroy for the next
            // frame), so a deleted Skia/GskGpu program stops holding host residency and a recycled GL program
            // name cannot collide with the dead program's cached ids. See `GlContext::retire_program`.
            self.retire_program(program);
            if self.local.cur_prog == program {
                self.local.cur_prog = 0;
            }
        }
    }

    /// `glDeleteShader(shader)` — ES 3.0 §7.1: a shader still attached to a program is only flagged; its
    /// source and compile status survive until the last `glDetachShader`.
    pub fn delete_shader(&mut self, shader: u32) {
        self.programs.delete_shader(shader);
    }
}

pub const DELETE_PROGRAM: fn(&mut GlContext, u32) = GlContext::delete_program;
pub const DELETE_SHADER: fn(&mut GlContext, u32) = GlContext::delete_shader;
pub use DELETE_PROGRAM as delete_program;
pub use DELETE_SHADER as delete_shader;

/// `glDetachShader(program, shader)` — clear the matching attachment slot. Honest GL errors: an unknown
/// program or shader → `GL_INVALID_VALUE`; a shader not attached to the program → `GL_INVALID_OPERATION`.
pub fn detach_shader(ctx: &mut GlContext, program: u32, shader: u32) {
    if !ctx.programs.contains(program) {
        ctx.set_gl_error(if ctx.programs.has_shader(program) {
            GL_INVALID_OPERATION
        } else {
            GL_INVALID_VALUE
        });
        return;
    }
    if !ctx.programs.shader_exists(shader) {
        ctx.set_gl_error(if ctx.programs.contains(shader) {
            GL_INVALID_OPERATION
        } else {
            GL_INVALID_VALUE
        });
        return;
    }
    if !ctx.programs.detach(program, shader) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    }
}

/// Snapshot the currently-bound draw state into a fresh [`DrawCall`] (the immutable per-draw record).
impl GlContext {
    /// The preconditions every `glDraw*` shares, in the order ES 3.0 §2.8.3 raises them. Returns `false`
    /// when the draw must record nothing, having raised the error itself.
    ///
    /// * an undefined primitive `mode` → `GL_INVALID_ENUM`;
    /// * an index type other than `GL_UNSIGNED_{BYTE,SHORT,INT}` → `GL_INVALID_ENUM`;
    /// * a draw framebuffer that is not complete → `GL_INVALID_FRAMEBUFFER_OPERATION`.
    ///
    /// Each was previously accepted silently, which is worse than a wrong picture: an application that
    /// checks `glGetError` to decide whether its framebuffer is usable was told it was.
    pub(super) fn draw_preconditions(&mut self, mode: u32, index_type: Option<u32>) -> bool {
        // GL_POINTS(0) … GL_TRIANGLE_FAN(6) are the seven defined modes; there is no glconst for
        // GL_LINE_LOOP(2) or GL_TRIANGLE_FAN(6).
        if mode > 6 {
            self.set_gl_error(GL_INVALID_ENUM);
            return false;
        }
        if let Some(index_type) = index_type {
            if !matches!(
                index_type,
                GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT | GL_UNSIGNED_INT
            ) {
                self.set_gl_error(GL_INVALID_ENUM);
                return false;
            }
        }
        // Completeness is asked of USER framebuffers only. The default framebuffer's is EGL's business,
        // and a surfaceless context legitimately renders to its FBOs — `build_raw` already declines the
        // default-framebuffer work such a context records, so testing it here would only reject draws
        // this driver serves correctly today.
        let fbo = self.local.bound_fbo;
        if fbo != 0 && self.framebuffer_status(fbo) != GL_FRAMEBUFFER_COMPLETE {
            self.set_gl_error(GL_INVALID_FRAMEBUFFER_OPERATION);
            return false;
        }
        // ES 3.0 §2.8: with a NON-DEFAULT vertex array object bound, an enabled attribute array whose
        // buffer binding is zero is GL_INVALID_OPERATION. There is no client-array fallback for a non-
        // default VAO — client arrays are legal only on the default one — so this draw has no vertex
        // source at all.
        //
        // This is the error neither implementation raised, and letting the draw through is what destroyed
        // the context: it reached the GPU transport, failed there, and a transport failure marks the whole
        // share group LOST. A lost group makes every later GL call return `R::default()` without reaching
        // the model — which is why `glCheckFramebufferStatus` answered 0x0000 (not a value it can return),
        // the error queue stayed empty, and neither a fresh buffer nor a plain clear could recover it.
        if self.local.cur_vao != 0
            && self
                .local
                .attr
                .iter()
                .any(|attr| attr.enabled && attr.buffer == 0 && attr.binding.is_none())
        {
            self.set_gl_error(GL_INVALID_OPERATION);
            return false;
        }
        true
    }

    pub(super) fn draw_uses_mapped_buffer(&self) -> bool {
        self.local.attr.iter().any(|attribute| {
            if !attribute.enabled {
                return false;
            }
            let buffer = attribute
                .binding
                .and_then(|binding| self.local.vertex_bindings.get(binding as usize))
                .map(|binding| binding.buffer)
                .filter(|buffer| *buffer != 0)
                .unwrap_or(attribute.buffer);
            buffer != 0 && self.buffers.is_mapped(buffer)
        }) || (self.local.element_buffer != 0 && self.buffers.is_mapped(self.local.element_buffer))
            || self
                .local
                .indexed_buffers
                .iter()
                .any(|(&(target, _), binding)| {
                    target == GL_UNIFORM_BUFFER && self.buffers.is_mapped(binding.buffer)
                })
    }

    pub(super) fn snapshot(&self, capture_buffers: bool) -> DrawCall {
        let ctx = self;
        // Capture the ES3 sampler OBJECT bound to each texture unit: a bound object overrides the texture's own
        // filter/wrap at lowering time (ES 3.0 §3.8.13). `None` where no object is bound (texture params win).
        let mut samp_objs: [Option<crate::model::es3::SamplerObj>; MAX_TEXTURE_UNITS] =
            [None; MAX_TEXTURE_UNITS];
        for (unit, slot) in samp_objs.iter_mut().enumerate() {
            let name = ctx.samplers.binding(unit as u32);
            if name != 0 {
                *slot = ctx.samplers.get(name).copied();
            }
        }
        let mut attrs = ctx.local.attr;
        for attr in &mut attrs {
            let Some(binding) = attr.binding else {
                continue;
            };
            let Some(slot) = ctx.local.vertex_bindings.get(binding as usize) else {
                continue;
            };
            attr.buffer = slot.buffer;
            attr.offset = slot.offset.saturating_add(attr.offset);
            attr.stride = slot.stride;
            attr.divisor = slot.divisor;
        }
        let mut d = DrawCall {
            prog: ctx.local.cur_prog,
            fbo: ctx.local.bound_fbo,
            attrs,
            current_attrs: ctx.local.current_attr,
            current_attr_kinds: ctx.local.current_attr_kind,
            tex_units: ctx.local.tex_unit,
            samp_objs,
            viewport: ctx.local.pipeline.viewport,
            scissor_enabled: ctx.local.pipeline.scissor_enabled,
            scissor: ctx.local.pipeline.scissor,
            rasterizer_discard: ctx.local.pipeline.rasterizer_discard,
            blend: ctx.local.pipeline.blend,
            blend_src_rgb: ctx.local.pipeline.blend_src_rgb,
            blend_dst_rgb: ctx.local.pipeline.blend_dst_rgb,
            blend_src_alpha: ctx.local.pipeline.blend_src_alpha,
            blend_dst_alpha: ctx.local.pipeline.blend_dst_alpha,
            blend_eq_rgb: ctx.local.pipeline.blend_eq_rgb,
            blend_eq_alpha: ctx.local.pipeline.blend_eq_alpha,
            blend_color: ctx.local.pipeline.blend_color,
            // ES 3.0 §4.1.5/§4.1.6: with no depth (resp. stencil) attachment on the DRAW framebuffer the
            // test behaves as though it always passes and nothing is written — it is not the enable alone
            // that arms the test. The default framebuffer's planes come from the context's `EGLConfig`.
            depth: ctx.local.pipeline.depth && ctx.draw_framebuffer_has_depth(),
            depth_func: ctx.local.pipeline.depth_func,
            depth_write: ctx.local.pipeline.depth_write && ctx.draw_framebuffer_has_depth(),
            stencil: ctx.local.pipeline.stencil && ctx.draw_framebuffer_has_stencil(),
            stencil_func_front: ctx.local.pipeline.stencil_func_front,
            stencil_func_back: ctx.local.pipeline.stencil_func_back,
            stencil_fail_front: ctx.local.pipeline.stencil_fail_front,
            stencil_zfail_front: ctx.local.pipeline.stencil_zfail_front,
            stencil_zpass_front: ctx.local.pipeline.stencil_zpass_front,
            stencil_fail_back: ctx.local.pipeline.stencil_fail_back,
            stencil_zfail_back: ctx.local.pipeline.stencil_zfail_back,
            stencil_zpass_back: ctx.local.pipeline.stencil_zpass_back,
            stencil_ref_front: ctx.local.pipeline.stencil_ref_front,
            stencil_ref_back: ctx.local.pipeline.stencil_ref_back,
            stencil_read_mask_front: ctx.local.pipeline.stencil_read_mask_front,
            stencil_read_mask_back: ctx.local.pipeline.stencil_read_mask_back,
            stencil_write_mask_front: ctx.local.pipeline.stencil_write_mask_front,
            stencil_write_mask_back: ctx.local.pipeline.stencil_write_mask_back,
            cull_enabled: ctx.local.pipeline.cull_enabled,
            cull_face: ctx.local.pipeline.cull_face,
            front_face: ctx.local.pipeline.front_face,
            color_mask: ctx.local.pipeline.color_mask,
            depth_range: ctx.local.pipeline.depth_range,
            draw_buffer_mask: ctx.draw_buffer_mask(),
            clear: ctx.local.pipeline.clear_color.map(f64::from),
            clear_depth: ctx.local.pipeline.clear_depth,
            clear_stencil: ctx.local.pipeline.clear_stencil,
            elem_buf: ctx.local.element_buffer,
            ..DrawCall::default()
        };
        d.target = if ctx.local.bound_fbo == 0 {
            None
        } else {
            let texture = ctx.local.framebuffers.color_attachment(ctx.local.bound_fbo);
            ctx.textures
                .get(texture)
                .filter(|t| t.w > 0 && t.h > 0)
                .map(|t| crate::model::program::TargetSnapshot {
                    texture,
                    generation: t.gen,
                    shared_storage: t.shared_storage(),
                    shared_revision: t.shared_current_identity().map(|(_, revision)| revision),
                    width: t.w,
                    height: t.h,
                    format: t.ir_format,
                })
        };
        for unit in 0..d.tex_units.len() {
            if let Some(texture) = ctx.textures.get(d.tex_units[unit]) {
                d.tex_generations[unit] = texture.gen;
                d.tex_swizzles[unit] = texture.swizzle;
            }
        }
        if let Some(p) = ctx.programs.program(ctx.local.cur_prog) {
            d.samp_units.clone_from(&p.samp_units);
            // Snapshot the default-block `glUniform*` bytes for THIS draw: `Program::ubuf` is mutable state,
            // so a later draw that changes a uniform must not retroactively alter this draw's bytes.
            let sz = p.ubuf_size.max(0) as usize;
            if sz > 0 {
                d.ubuf_bytes = p.ubuf[..sz.min(p.ubuf.len())].to_vec();
            }
        }
        d.textures = d
            .samp_units
            .iter()
            .copied()
            .filter(|unit| (0..d.tex_units.len() as i32).contains(unit))
            .map(|unit| d.tex_units[unit as usize])
            .filter(|name| *name != 0)
            .filter_map(|name| ctx.texture_snapshot(name))
            .collect();
        d.textures
            .sort_unstable_by_key(|snapshot| (snapshot.name, snapshot.generation));
        d.textures
            .dedup_by_key(|snapshot| (snapshot.name, snapshot.generation));
        if !capture_buffers {
            return d;
        }
        d.ubo_bytes = self.resolve_block_ubo_bytes(ctx.local.cur_prog);
        let mut names: Vec<u32> = d
            .attrs
            .iter()
            .filter(|attr| attr.enabled && attr.buffer != 0)
            .map(|attr| attr.buffer)
            .collect();
        if d.elem_buf != 0 {
            names.push(d.elem_buf);
        }
        names.sort_unstable();
        names.dedup();
        d.buffers = names
            .into_iter()
            .filter_map(|name| {
                ctx.buffers
                    .get(name)
                    .map(|buffer| crate::model::program::BufferSnapshot {
                        name,
                        generation: buffer.gen,
                        data: buffer.data.clone(),
                    })
            })
            .collect();
        d
    }
}

/// Resolve the app's uniform-BLOCK bytes for `prog_name` at draw time — the std140 data the shader's
/// `layout(std140, binding = 0) uniform … { … }` block reads. The chain is:
/// `glBindBufferBase(GL_UNIFORM_BUFFER, blockBinding, buffer)` bound a buffer to the block's binding point,
/// and `glBufferData`/`glBufferSubData` filled it. We locate the block's binding point, then the indexed
/// UBO binding at that point, then that buffer's bytes.
///
/// Binding-point priority: the shader's explicit `layout(binding = N)` qualifier (GskGpu/GTK4 declares
/// `binding = 0` in-shader and binds via `glBindBufferBase`), else an app-assigned `glUniformBlockBinding`
/// value, else `0`. Returns EMPTY when the program has no data uniforms, declares no block, or has no UBO
/// bound at the resolved point (the default-uniform `glUniform*` path — the caller then keeps `Program::ubuf`).
impl GlContext {
    pub(super) fn resolve_block_ubo_bytes(&self, prog_name: u32) -> Vec<u8> {
        let ctx = self;
        let prog = match ctx.programs.program(prog_name) {
            Some(p) if p.has_uniforms() => p,
            _ => return Vec::new(),
        };
        // MULTI-BLOCK program: the shader declares 2+ uniform blocks, each at its OWN binding point fed by its
        // OWN `glBindBufferRange`d range. The translator flattens every block's members into ONE `HlUniforms`
        // std140 block at IR binding 0 (declaration order — see `adapter::glsl::translate_render`), so the
        // recorded binding-0 bytes are assembled block-by-block: each block contributes its own bound range's
        // std140 bytes, 16-byte aligned to the next block (matching std140 for the vec4/mat-member blocks
        // GskGpu-style programs use). This proves each `glBindBufferRange` fed the right binding.
        let blocks =
            crate::adapter::glsl::StageSources::new(&prog.vs_src, &prog.fs_src).uniform_blocks();
        if blocks.len() >= 2 {
            return self.assemble_multi_block_ubo_bytes(&blocks);
        }
        // The block's binding point (see priority above).
        let bp = crate::adapter::glsl::Source::new(&prog.vs_src)
            .uniform_block_binding()
            .or_else(|| crate::adapter::glsl::Source::new(&prog.fs_src).uniform_block_binding())
            .or_else(|| {
                ctx.uniform_blocks
                    .get(&prog_name)
                    .and_then(|blocks| blocks.first())
                    .map(|b| b.binding)
            })
            .unwrap_or(0);
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "[UBO_DUMP] prog={prog_name} has_uniforms=true ubuf_size={} bp={bp} indexed_keys={:?}",
            prog.ubuf_size,
            ctx.local.indexed_buffers.keys().collect::<Vec<_>>()
        );
        let ib = match ctx.local.indexed_buffers.get(&(GL_UNIFORM_BUFFER, bp)) {
            Some(ib) => *ib,
            None => return Vec::new(),
        };
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "[UBO_DUMP] ib buffer={} off={} size={} bufbytes={} head={:?}",
            ib.buffer,
            ib.offset,
            ib.size,
            ctx.buffers
                .get(ib.buffer)
                .map(|buffer| buffer.data.len())
                .unwrap_or(0),
            ctx.buffers
                .get(ib.buffer)
                .map(|buffer| buffer.data.iter().take(16).copied().collect::<Vec<_>>())
                .unwrap_or_default()
        );
        let buf = match ctx.buffers.get(ib.buffer) {
            Some(b) => b,
            None => return Vec::new(),
        };
        let off = ib.offset.max(0) as usize;
        if off >= buf.data.len() {
            return Vec::new();
        }
        // `size == 0` (from `glBindBufferBase`) means the whole buffer from `offset`.
        let end = if ib.size <= 0 {
            buf.data.len()
        } else {
            (off + ib.size as usize).min(buf.data.len())
        };
        buf.data[off..end].to_vec()
    }

    /// Assemble the flattened `HlUniforms` binding-0 bytes for a MULTI-block program from each block's own
    /// `glBindBufferRange`d range, in `blocks` (declaration) order. Each block appends its bound range's std140
    /// bytes, then pads to the next 16-byte boundary so the following block starts 16-aligned (std140 for a
    /// vec4/mat4-member block). A block with no bound range contributes a zero-filled std140 span (an honest
    /// hole, not a fake). This is what routes two ranges to two distinct binding points through the single
    /// flattened block the translator emits.
    pub(super) fn assemble_multi_block_ubo_bytes(
        &self,
        blocks: &[crate::adapter::glsl::UniformBlockDecl],
    ) -> Vec<u8> {
        let ctx = self;
        let mut out: Vec<u8> = Vec::new();
        for blk in blocks {
            let bytes = ctx
                .local
                .indexed_buffers
                .get(&(GL_UNIFORM_BUFFER, blk.binding))
                .and_then(|ib| {
                    let buf = ctx.buffers.get(ib.buffer)?;
                    let off = ib.offset.max(0) as usize;
                    if off > buf.data.len() {
                        return Some(Vec::new());
                    }
                    let end = if ib.size <= 0 {
                        buf.data.len()
                    } else {
                        (off + ib.size as usize).min(buf.data.len())
                    };
                    Some(buf.data[off..end].to_vec())
                })
                .unwrap_or_default();
            out.extend_from_slice(&bytes);
            // Pad this block's contribution up to the next 16-byte std140 boundary (each block is 16-aligned).
            while !out.len().is_multiple_of(16) {
                out.push(0);
            }
        }
        out
    }
}
