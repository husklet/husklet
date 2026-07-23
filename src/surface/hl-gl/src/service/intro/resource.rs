//! Program-resource introspection (`glGetProgramInterfaceiv` and `glGetProgramResource*`).

use crate::adapter::glsl;
use crate::model::context::GlContext;
use crate::model::glconst::*;
use crate::service::query::gl_type_enum;

/// One introspected program resource (a uniform / input / output).
pub struct Resource {
    pub name: String,
    pub gl_type: u32,
    /// The GL location (`glGetProgramResourceLocation`) — a uniform's/attribute's declaration index, an
    /// output's fragment-data location. `-1` when the interface has no location namespace (uniform blocks).
    pub location: i32,
}

/// The active resources of a program `interface` in enumeration order (the order `glGetProgramResourceName`
/// indexes and `glGetProgramInterfaceiv(GL_ACTIVE_RESOURCES)` counts). Empty for an unknown/unlinked
/// program or an interface this model does not reflect.
pub fn interface_resources(ctx: &GlContext, program: u32, interface: u32) -> Vec<Resource> {
    let Some(p) = ctx.programs.program(program) else {
        return Vec::new();
    };
    if !p.linked {
        return Vec::new();
    }
    match interface {
        GL_UNIFORM => {
            let data = glsl::StageSources::new(&p.vs_src, &p.fs_src).uniform_decls();
            let samps = glsl::StageSources::new(&p.vs_src, &p.fs_src).sampler_decls();
            data.into_iter()
                .enumerate()
                .map(|(i, d)| Resource {
                    name: d.name.clone(),
                    gl_type: gl_type_enum(&d.ty),
                    location: i as i32,
                })
                // A sampler uniform has no default-block location in the value sense, but the resource
                // location namespace mirrors `glGetUniformLocation` (declaration index within its kind).
                .chain(samps.into_iter().enumerate().map(|(i, d)| Resource {
                    name: d.name.clone(),
                    gl_type: gl_type_enum(&d.ty),
                    location: i as i32,
                }))
                .collect()
        }
        GL_PROGRAM_INPUT => glsl::Source::new(&p.vs_src)
            .vertex_attrs()
            .into_iter()
            .enumerate()
            .map(|(i, d)| Resource {
                name: d.name.clone(),
                gl_type: gl_type_enum(&d.ty),
                location: i as i32,
            })
            .collect(),
        GL_PROGRAM_OUTPUT => glsl::StageSources::new("", &p.fs_src)
            .frag_outputs()
            .into_iter()
            .enumerate()
            .map(|(i, d)| Resource {
                name: d.name.clone(),
                gl_type: gl_type_enum(&d.ty),
                location: i as i32,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `glGetProgramInterfaceiv(program, interface, pname)` — the interface's active-resource count / the
/// longest resource name length + 1. `None` for a pname this model does not reflect.
pub fn program_interfaceiv(
    ctx: &GlContext,
    program: u32,
    interface: u32,
    pname: u32,
) -> Option<i32> {
    if interface == GL_UNIFORM_BLOCK {
        // Uniform blocks are reflected through the block table (block 0 = the implicit block).
        let n = match ctx.programs.program(program) {
            Some(program) if program.has_uniforms() => 1,
            _ => 0,
        };
        let max_name_length = if n > 0 {
            "Uniforms".len() as i32 + 1
        } else {
            0
        };
        return Some(match pname {
            GL_ACTIVE_RESOURCES => n,
            GL_MAX_NAME_LENGTH => max_name_length,
            _ => 0,
        });
    }
    let res = interface_resources(ctx, program, interface);
    Some(match pname {
        GL_ACTIVE_RESOURCES => res.len() as i32,
        GL_MAX_NAME_LENGTH => res
            .iter()
            .map(|r| r.name.len() as i32 + 1)
            .max()
            .unwrap_or(0),
        _ => 0,
    })
}

/// `glGetProgramResourceIndex(program, interface, name)` — the enumeration index of the named resource, or
/// `GL_INVALID_INDEX` if not found.
pub fn program_resource_index(ctx: &GlContext, program: u32, interface: u32, name: &str) -> u32 {
    interface_resources(ctx, program, interface)
        .iter()
        .position(|r| r.name == name)
        .map(|i| i as u32)
        .unwrap_or(GL_INVALID_INDEX)
}

/// `glGetProgramResourceLocation(program, interface, name)` — the GL location of the named uniform /
/// input / output, or `-1` if not found.
pub fn program_resource_location(ctx: &GlContext, program: u32, interface: u32, name: &str) -> i32 {
    interface_resources(ctx, program, interface)
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.location)
        .unwrap_or(-1)
}

/// `glGetProgramResourceName(program, interface, index)` — the resource's declared name, or `None`.
pub fn program_resource_name(
    ctx: &GlContext,
    program: u32,
    interface: u32,
    index: u32,
) -> Option<String> {
    interface_resources(ctx, program, interface)
        .into_iter()
        .nth(index as usize)
        .map(|r| r.name)
}

/// `glGetProgramResourceiv(program, interface, index, prop)` — one queried property of the resource. `None`
/// for an out-of-range index (the caller writes nothing for that slot).
pub fn program_resourceiv(
    ctx: &GlContext,
    program: u32,
    interface: u32,
    index: u32,
    prop: u32,
) -> Option<i32> {
    let res = interface_resources(ctx, program, interface);
    let r = res.get(index as usize)?;
    Some(match prop {
        GL_TYPE => r.gl_type as i32,
        GL_ARRAY_SIZE => 1,
        GL_NAME_LENGTH => r.name.len() as i32 + 1,
        GL_LOCATION => r.location,
        GL_OFFSET => -1,
        GL_BLOCK_INDEX => -1,
        _ => 0,
    })
}
