use super::*;
// ==================================================================================================
// GLES3.1: glProgramUniform* (DSA) — write into the NAMED program's uniform block (no bind required)
// ==================================================================================================

/// Write `bytes` into data uniform `location` of `program` (the DSA setter core).
fn set_program_uniform(program: u32, location: i32, bytes: &[u8]) {
    GlobalState::context(|s| record::program_uniform_at(&mut s.gl, program, location, bytes));
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1i(program: u32, location: i32, v0: i32) {
    if location < 0 {
        return;
    }
    GlobalState::context(|s| record::program_uniform_i32_at(&mut s.gl, program, location, &[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2i(program: u32, location: i32, v0: i32, v1: i32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3i(program: u32, location: i32, v0: i32, v1: i32, v2: i32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4i(
    program: u32,
    location: i32,
    v0: i32,
    v1: i32,
    v2: i32,
    v3: i32,
) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1ui(program: u32, location: i32, v0: u32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2ui(program: u32, location: i32, v0: u32, v1: u32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3ui(program: u32, location: i32, v0: u32, v1: u32, v2: u32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4ui(
    program: u32,
    location: i32,
    v0: u32,
    v1: u32,
    v2: u32,
    v3: u32,
) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1f(program: u32, location: i32, v0: f32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2f(program: u32, location: i32, v0: f32, v1: f32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3f(program: u32, location: i32, v0: f32, v1: f32, v2: f32) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1, v2]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4f(
    program: u32,
    location: i32,
    v0: f32,
    v1: f32,
    v2: f32,
    v3: f32,
) {
    set_program_uniform(program, location, &LittleEndian::encode(&[v0, v1, v2, v3]));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 1) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 2) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 3) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4fv(program: u32, location: i32, count: i32, value: *const f32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_f32(value, count, 4) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1iv(program: u32, location: i32, count: i32, value: *const i32) {
    let values = unsafe { slice_i32(value, count, 1) };
    GlobalState::context(|state| {
        record::program_uniform_i32_at(&mut state.gl, program, location, values)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 2) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 3) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4iv(program: u32, location: i32, count: i32, value: *const i32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_i32(value, count, 4) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform1uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 1) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform2uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 2) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform3uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 3) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniform4uiv(program: u32, location: i32, count: i32, value: *const u32) {
    set_program_uniform(
        program,
        location,
        &LittleEndian::encode(unsafe { slice_u32(value, count, 4) }),
    );
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix2fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(2, 2, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix3fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(3, 3, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix4fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(4, 4, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix2x3fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(2, 3, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix3x2fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(3, 2, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix2x4fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(2, 4, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix4x2fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(4, 2, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix3x4fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(3, 4, count, transpose != 0, value)
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glProgramUniformMatrix4x3fv(
    program: u32,
    location: i32,
    count: i32,
    transpose: u8,
    value: *const f32,
) {
    set_program_uniform(program, location, &unsafe {
        mat_bytes_cr(4, 3, count, transpose != 0, value)
    });
}
