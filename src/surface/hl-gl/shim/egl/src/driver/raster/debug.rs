// ==================================================================================================
// KHR_debug: message log + object labels — no debug state is modeled (honest empty/no-op)
// ==================================================================================================

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageCallback(_callback: *mut c_void, _user_param: *const c_void) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageCallbackKHR(callback: *mut c_void, user_param: *const c_void) {
    glDebugMessageCallback(callback, user_param);
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageControl(
    _source: u32,
    _type_: u32,
    _severity: u32,
    _count: i32,
    _ids: *const u32,
    _enabled: u8,
) {
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageControlKHR(
    source: u32,
    type_: u32,
    severity: u32,
    count: i32,
    ids: *const u32,
    enabled: u8,
) {
    glDebugMessageControl(source, type_, severity, count, ids, enabled);
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageInsert(
    _source: u32,
    _type_: u32,
    _id: u32,
    _severity: u32,
    _length: i32,
    _buf: *const c_char,
) {
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageInsertKHR(
    source: u32,
    type_: u32,
    id: u32,
    severity: u32,
    length: i32,
    buf: *const c_char,
) {
    glDebugMessageInsert(source, type_, id, severity, length, buf);
}
/// `glGetDebugMessageLog` — no messages are recorded (this driver logs GL diagnostics out-of-band), so it
/// returns 0 messages.
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetDebugMessageLog(
    _count: u32,
    _buf_size: i32,
    _sources: *mut u32,
    _types: *mut u32,
    _ids: *mut u32,
    _severities: *mut u32,
    _lengths: *mut i32,
    _message_log: *mut c_char,
) -> u32 {
    0
}
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetDebugMessageLogKHR(
    count: u32,
    buf_size: i32,
    sources: *mut u32,
    types: *mut u32,
    ids: *mut u32,
    severities: *mut u32,
    lengths: *mut i32,
    message_log: *mut c_char,
) -> u32 {
    glGetDebugMessageLog(
        count,
        buf_size,
        sources,
        types,
        ids,
        severities,
        lengths,
        message_log,
    )
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPushDebugGroup(_source: u32, _id: u32, _length: i32, _message: *const c_char) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPushDebugGroupKHR(source: u32, id: u32, length: i32, message: *const c_char) {
    glPushDebugGroup(source, id, length, message);
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPopDebugGroup() {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPopDebugGroupKHR() {
    glPopDebugGroup();
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectLabel(_identifier: u32, _name: u32, _length: i32, _label: *const c_char) {
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectLabelKHR(identifier: u32, name: u32, length: i32, label: *const c_char) {
    glObjectLabel(identifier, name, length, label);
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectPtrLabel(_ptr: *const c_void, _length: i32, _label: *const c_char) {}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectPtrLabelKHR(ptr: *const c_void, length: i32, label: *const c_char) {
    glObjectPtrLabel(ptr, length, label);
}
/// `glGetObjectLabel` / `glGetObjectPtrLabel` — no labels are stored: report an empty label (length 0).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectLabel(
    _identifier: u32,
    _name: u32,
    buf_size: i32,
    length: *mut i32,
    label: *mut c_char,
) {
    unsafe { write_c_name(&[], buf_size, length, label) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectLabelKHR(
    identifier: u32,
    name: u32,
    buf_size: i32,
    length: *mut i32,
    label: *mut c_char,
) {
    glGetObjectLabel(identifier, name, buf_size, length, label);
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectPtrLabel(
    _ptr: *const c_void,
    buf_size: i32,
    length: *mut i32,
    label: *mut c_char,
) {
    unsafe { write_c_name(&[], buf_size, length, label) };
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectPtrLabelKHR(
    ptr: *const c_void,
    buf_size: i32,
    length: *mut i32,
    label: *mut c_char,
) {
    glGetObjectPtrLabel(ptr, buf_size, length, label);
}

// ==================================================================================================
// shader binary / compiler control — no shader-binary formats advertised (honest)
// ==================================================================================================

/// `glReleaseShaderCompiler` — a hint that the compiler may free resources. This driver compiles from
/// source at link (`GL_SHADER_COMPILER` == true), so there is nothing to release: an honest no-op.
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glReleaseShaderCompiler() {}
/// `glShaderBinary(...)` — no shader-binary formats are advertised (`GL_NUM_SHADER_BINARY_FORMATS` == 0),
/// so a binary load is rejected as `GL_INVALID_ENUM` (the app must supply GLSL source).
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glShaderBinary(
    _count: i32,
    _shaders: *const u32,
    _binaryformat: u32,
    _binary: *const c_void,
    _length: i32,
) {
    GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_ENUM));
}
use super::*;
