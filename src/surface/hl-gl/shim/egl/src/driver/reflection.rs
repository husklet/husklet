use super::*;
// ==================================================================================================
// GLES3.0: uniform value readback (glGetUniform* / glGetnUniform*)
// ==================================================================================================

/// Convert the uniform at `location` to the getter's requested scalar type and write as many complete
/// elements as fit. Unknown values leave one zero, matching the existing fail-soft ABI behavior.
unsafe fn read_uniform<T: Copy>(
    program: u32,
    location: i32,
    out: *mut T,
    max_bytes: usize,
    read: impl FnOnce(&GlContext, u32, i32) -> Option<Vec<T>>,
) {
    if out.is_null() {
        return;
    }
    let values = GlobalState::context(|s| read(&s.gl, program, location));
    match values {
        Some(values) => {
            let count = values.len().min(max_bytes / core::mem::size_of::<T>());
            core::ptr::copy_nonoverlapping(values.as_ptr(), out, count);
        }
        None => {
            if max_bytes >= core::mem::size_of::<T>() {
                core::ptr::write_bytes(out, 0, 1);
            }
        }
    }
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformfv(program: u32, location: i32, params: *mut f32) {
    unsafe { read_uniform(program, location, params, usize::MAX, intro::get_uniform_f32) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformiv(program: u32, location: i32, params: *mut i32) {
    unsafe { read_uniform(program, location, params, usize::MAX, intro::get_uniform_i32) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformuiv(program: u32, location: i32, params: *mut u32) {
    unsafe { read_uniform(program, location, params, usize::MAX, intro::get_uniform_u32) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetnUniformfv(program: u32, location: i32, buf_size: i32, params: *mut f32) {
    unsafe {
        read_uniform(program, location, params, buf_size.max(0) as usize, intro::get_uniform_f32)
    };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetnUniformiv(program: u32, location: i32, buf_size: i32, params: *mut i32) {
    unsafe {
        read_uniform(program, location, params, buf_size.max(0) as usize, intro::get_uniform_i32)
    };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetnUniformuiv(program: u32, location: i32, buf_size: i32, params: *mut u32) {
    unsafe {
        read_uniform(program, location, params, buf_size.max(0) as usize, intro::get_uniform_u32)
    };
}

// ==================================================================================================
// GLES3.0: uniform-block reflection (glGetUniformIndices / glGetActiveUniforms* / glUniformBlock*)
// ==================================================================================================

/// `glGetUniformIndices(program, count, names, indices)` — resolve each uniform name to its active index
/// (or `GL_INVALID_INDEX`). Real: keyed on the program's reflected uniform table.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformIndices(
    program: u32,
    uniform_count: i32,
    uniform_names: *const *const c_char,
    uniform_indices: *mut u32,
) {
    if uniform_indices.is_null() || uniform_count <= 0 {
        return;
    }
    for i in 0..uniform_count as isize {
        let idx = if uniform_names.is_null() {
            GL_INVALID_INDEX
        } else {
            match unsafe { Text::read(*uniform_names.offset(i)) } {
                Some(name) => GlobalState::context(|s| intro::uniform_index(&s.gl, program, &name)),
                None => GL_INVALID_INDEX,
            }
        };
        unsafe { *uniform_indices.offset(i) = idx };
    }
}

/// `glGetActiveUniformsiv(program, count, indices, pname, params)` — one reflected property per named
/// uniform index (type/size/offset/name-length/block-index), from the program's reflected tables.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformsiv(
    program: u32,
    uniform_count: i32,
    uniform_indices: *const u32,
    pname: u32,
    params: *mut i32,
) {
    if params.is_null() || uniform_indices.is_null() || uniform_count <= 0 {
        return;
    }
    for i in 0..uniform_count as isize {
        let index = unsafe { *uniform_indices.offset(i) };
        let v = GlobalState::context(|s| intro::active_uniformsiv(&s.gl, program, index, pname))
            .unwrap_or(0);
        unsafe { *params.offset(i) = v };
    }
}

/// `glGetUniformBlockIndex(program, name)` — the named uniform block's index (lazily assigned, stable),
/// or `GL_INVALID_INDEX`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetUniformBlockIndex(program: u32, uniform_block_name: *const c_char) -> u32 {
    let name = match unsafe { Text::read(uniform_block_name) } {
        Some(n) => n,
        None => return GL_INVALID_INDEX,
    };
    GlobalState::context(|s| intro::uniform_block_index(&mut s.gl, program, &name))
}

/// `glUniformBlockBinding(program, blockIndex, binding)` — assign the block's binding point (real state).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glUniformBlockBinding(
    program: u32,
    uniform_block_index: u32,
    uniform_block_binding: u32,
) {
    GlobalState::context(|s| {
        intro::uniform_block_binding(
            &mut s.gl,
            program,
            uniform_block_index,
            uniform_block_binding,
        )
    });
}

/// `glGetActiveUniformBlockiv(program, blockIndex, pname, params)` — binding / data size / active-uniform
/// count / name length of the block. Out-of-range block → `GL_INVALID_VALUE`.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformBlockiv(
    program: u32,
    uniform_block_index: u32,
    pname: u32,
    params: *mut i32,
) {
    let v = GlobalState::context(|s| {
        intro::active_uniform_blockiv(&mut s.gl, program, uniform_block_index, pname)
    });
    match v {
        Some(v) => {
            if !params.is_null() {
                unsafe { *params = v };
            }
        }
        None => GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE)),
    }
}

/// `glGetActiveUniformBlockName(program, blockIndex, bufSize, length, name)` — the block's declared name.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetActiveUniformBlockName(
    program: u32,
    uniform_block_index: u32,
    buf_size: i32,
    length: *mut i32,
    uniform_block_name: *mut c_char,
) {
    let name = GlobalState::context(|s| {
        intro::active_uniform_block_name(&mut s.gl, program, uniform_block_index)
    });
    match name {
        Some(n) => unsafe { write_c_name(n.as_bytes(), buf_size, length, uniform_block_name) },
        None => {
            GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE));
            unsafe { write_c_name(&[], buf_size, length, uniform_block_name) };
        }
    }
}

// ==================================================================================================
// GLES3.1: program-resource introspection (glGetProgramInterfaceiv / glGetProgramResource*)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramInterfaceiv(
    program: u32,
    program_interface: u32,
    pname: u32,
    params: *mut i32,
) {
    if params.is_null() {
        return;
    }
    let v = GlobalState::context(|s| {
        intro::program_interfaceiv(&s.gl, program, program_interface, pname)
    })
    .unwrap_or(0);
    unsafe { *params = v };
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramResourceIndex(
    program: u32,
    program_interface: u32,
    name: *const c_char,
) -> u32 {
    let want = match unsafe { Text::read(name) } {
        Some(n) => n,
        None => return GL_INVALID_INDEX,
    };
    GlobalState::context(|s| {
        intro::program_resource_index(&s.gl, program, program_interface, &want)
    })
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramResourceLocation(
    program: u32,
    program_interface: u32,
    name: *const c_char,
) -> i32 {
    let want = match unsafe { Text::read(name) } {
        Some(n) => n,
        None => return -1,
    };
    GlobalState::context(|s| {
        intro::program_resource_location(&s.gl, program, program_interface, &want)
    })
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetProgramResourceName(
    program: u32,
    program_interface: u32,
    index: u32,
    buf_size: i32,
    length: *mut i32,
    name: *mut c_char,
) {
    let n = GlobalState::context(|s| {
        intro::program_resource_name(&s.gl, program, program_interface, index)
    });
    match n {
        Some(n) => unsafe { write_c_name(n.as_bytes(), buf_size, length, name) },
        None => {
            GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE));
            unsafe { write_c_name(&[], buf_size, length, name) };
        }
    }
}

/// `glGetProgramResourceiv(program, interface, index, propCount, props, bufSize, length, params)` — one
/// value per requested property of the resource (type / array size / name length / location).
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetProgramResourceiv(
    program: u32,
    program_interface: u32,
    index: u32,
    prop_count: i32,
    props: *const u32,
    buf_size: i32,
    length: *mut i32,
    params: *mut i32,
) {
    if props.is_null() || params.is_null() || prop_count <= 0 || buf_size <= 0 {
        if !length.is_null() {
            unsafe { *length = 0 };
        }
        return;
    }
    let cap = (prop_count as usize).min(buf_size as usize);
    let mut written = 0usize;
    for i in 0..cap {
        let prop = unsafe { *props.add(i) };
        let v = GlobalState::context(|s| {
            intro::program_resourceiv(&s.gl, program, program_interface, index, prop)
        })
        .unwrap_or(0);
        unsafe { *params.add(i) = v };
        written += 1;
    }
    if !length.is_null() {
        unsafe { *length = written as i32 };
    }
}
