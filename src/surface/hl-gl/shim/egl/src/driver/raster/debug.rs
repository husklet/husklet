use super::*;
use hl_gl::model::context::{
    DebugDelivery, DebugMessage, MAX_DEBUG_MESSAGE_LENGTH_VALUE, MAX_LABEL_LENGTH_VALUE,
};

type DebugCallback = unsafe extern "C" fn(u32, u32, u32, u32, i32, *const c_char, *const c_void);

enum InputBytes {
    Value(Vec<u8>),
    Null,
    TooLong,
}

unsafe fn input_bytes(pointer: *const c_char, length: i32, limit: usize) -> InputBytes {
    if pointer.is_null() {
        return InputBytes::Null;
    }
    if length >= 0 {
        let length = length as usize;
        if length >= limit {
            return InputBytes::TooLong;
        }
        return InputBytes::Value(
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) }.to_vec(),
        );
    }
    let mut bytes = Vec::new();
    while bytes.len() < limit {
        let byte = unsafe { *pointer.cast::<u8>().add(bytes.len()) };
        if byte == 0 {
            return InputBytes::Value(bytes);
        }
        bytes.push(byte);
    }
    InputBytes::TooLong
}

fn input_or_error(gl: &mut hl_gl::model::context::GlContext, input: InputBytes) -> Option<Vec<u8>> {
    match input {
        InputBytes::Value(bytes) => Some(bytes),
        InputBytes::Null | InputBytes::TooLong => {
            gl.set_gl_error(GL_INVALID_VALUE);
            None
        }
    }
}

fn dispatch(delivery: DebugDelivery) {
    let DebugDelivery::Callback {
        callback,
        user_param,
        message,
    } = delivery
    else {
        return;
    };
    let mut text = message.text;
    let length = text.len() as i32;
    text.push(0);
    let callback: DebugCallback = unsafe { std::mem::transmute(callback) };
    unsafe {
        callback(
            message.source,
            message.type_,
            message.id,
            message.severity,
            length,
            text.as_ptr(),
            user_param as *const c_void,
        )
    };
}

fn source(value: u32, any: bool) -> bool {
    (any && value == GL_DONT_CARE)
        || matches!(
            value,
            GL_DEBUG_SOURCE_API
                | GL_DEBUG_SOURCE_WINDOW_SYSTEM
                | GL_DEBUG_SOURCE_SHADER_COMPILER
                | GL_DEBUG_SOURCE_THIRD_PARTY
                | GL_DEBUG_SOURCE_APPLICATION
                | GL_DEBUG_SOURCE_OTHER
        )
}
fn type_(value: u32, any: bool) -> bool {
    (any && value == GL_DONT_CARE)
        || matches!(
            value,
            GL_DEBUG_TYPE_ERROR
                | GL_DEBUG_TYPE_DEPRECATED_BEHAVIOR
                | GL_DEBUG_TYPE_UNDEFINED_BEHAVIOR
                | GL_DEBUG_TYPE_PORTABILITY
                | GL_DEBUG_TYPE_PERFORMANCE
                | GL_DEBUG_TYPE_OTHER
                | GL_DEBUG_TYPE_MARKER
                | GL_DEBUG_TYPE_PUSH_GROUP
                | GL_DEBUG_TYPE_POP_GROUP
        )
}
fn insert_type(value: u32) -> bool {
    matches!(
        value,
        GL_DEBUG_TYPE_ERROR
            | GL_DEBUG_TYPE_DEPRECATED_BEHAVIOR
            | GL_DEBUG_TYPE_UNDEFINED_BEHAVIOR
            | GL_DEBUG_TYPE_PORTABILITY
            | GL_DEBUG_TYPE_PERFORMANCE
            | GL_DEBUG_TYPE_OTHER
            | GL_DEBUG_TYPE_MARKER
            | GL_DEBUG_TYPE_PUSH_GROUP
            | GL_DEBUG_TYPE_POP_GROUP
    )
}
fn severity(value: u32, any: bool) -> bool {
    (any && value == GL_DONT_CARE)
        || matches!(
            value,
            GL_DEBUG_SEVERITY_HIGH
                | GL_DEBUG_SEVERITY_MEDIUM
                | GL_DEBUG_SEVERITY_LOW
                | GL_DEBUG_SEVERITY_NOTIFICATION
        )
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageCallbackKHR(callback: *mut c_void, user: *const c_void) {
    GlobalState::context(|s| s.gl.set_debug_callback(callback as usize, user as usize));
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageCallback(callback: *mut c_void, user: *const c_void) {
    glDebugMessageCallbackKHR(callback, user)
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageControlKHR(
    source_: u32,
    type__: u32,
    severity_: u32,
    count: i32,
    ids: *const u32,
    enabled: u8,
) {
    GlobalState::context(|s| {
        if !source(source_, true) || !type_(type__, true) || !severity(severity_, true) {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return;
        }
        if count < 0 {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        if count > 0
            && (source_ == GL_DONT_CARE || type__ == GL_DONT_CARE || severity_ != GL_DONT_CARE)
        {
            s.gl.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let ids = if count == 0 {
            Vec::new()
        } else if ids.is_null() {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        } else {
            unsafe { std::slice::from_raw_parts(ids, count as usize) }.to_vec()
        };
        s.gl.debug_message_control(source_, type__, severity_, ids, enabled != 0);
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageControl(a: u32, b: u32, c: u32, d: i32, e: *const u32, f: u8) {
    glDebugMessageControlKHR(a, b, c, d, e, f)
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageInsertKHR(
    source_: u32,
    type__: u32,
    id: u32,
    severity_: u32,
    length: i32,
    buf: *const c_char,
) {
    let text = unsafe { input_bytes(buf, length, MAX_DEBUG_MESSAGE_LENGTH_VALUE) };
    let delivery = GlobalState::context(|s| {
        if !matches!(
            source_,
            GL_DEBUG_SOURCE_APPLICATION | GL_DEBUG_SOURCE_THIRD_PARTY
        ) || !insert_type(type__)
            || !severity(severity_, false)
        {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return None;
        }
        let Some(text) = input_or_error(&mut s.gl, text) else {
            return None;
        };
        Some(s.gl.deliver_debug_message(DebugMessage {
            source: source_,
            type_: type__,
            id,
            severity: severity_,
            text,
        }))
    });
    if let Some(delivery) = delivery {
        dispatch(delivery);
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glDebugMessageInsert(a: u32, b: u32, c: u32, d: u32, e: i32, f: *const c_char) {
    glDebugMessageInsertKHR(a, b, c, d, e, f)
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
    log: *mut c_char,
) -> u32 {
    if !log.is_null() && buf_size < 0 {
        GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE));
        return 0;
    }
    GlobalState::context(|s| {
        let mut written = 0usize;
        let mut offset = 0usize;
        while written < count as usize {
            let next = s.gl.next_debug_message_length();
            if next == 0 || (!log.is_null() && offset + next > buf_size as usize) {
                break;
            }
            let m = s.gl.take_debug_message().unwrap();
            unsafe {
                if !sources.is_null() {
                    *sources.add(written) = m.source
                }
                if !types.is_null() {
                    *types.add(written) = m.type_
                }
                if !ids.is_null() {
                    *ids.add(written) = m.id
                }
                if !severities.is_null() {
                    *severities.add(written) = m.severity
                }
                if !lengths.is_null() {
                    *lengths.add(written) = next as i32
                }
                if !log.is_null() {
                    std::ptr::copy_nonoverlapping(
                        m.text.as_ptr(),
                        log.add(offset).cast(),
                        m.text.len(),
                    );
                    *log.add(offset + m.text.len()) = 0
                }
            }
            if !log.is_null() {
                offset += next;
            }
            written += 1;
        }
        written as u32
    })
}
#[cfg_attr(gles_client, no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn glGetDebugMessageLog(
    a: u32,
    b: i32,
    c: *mut u32,
    d: *mut u32,
    e: *mut u32,
    f: *mut u32,
    g: *mut i32,
    h: *mut c_char,
) -> u32 {
    glGetDebugMessageLogKHR(a, b, c, d, e, f, g, h)
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPushDebugGroupKHR(source_: u32, id: u32, length: i32, message: *const c_char) {
    let text = unsafe { input_bytes(message, length, MAX_DEBUG_MESSAGE_LENGTH_VALUE) };
    let delivery = GlobalState::context(|s| {
        if !matches!(
            source_,
            GL_DEBUG_SOURCE_APPLICATION | GL_DEBUG_SOURCE_THIRD_PARTY
        ) {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return None;
        }
        let Some(text) = input_or_error(&mut s.gl, text) else {
            return None;
        };
        if !s.gl.debug_group_can_push() {
            s.gl.set_gl_error(GL_STACK_OVERFLOW);
            return None;
        }
        let push = DebugMessage {
            source: source_,
            type_: GL_DEBUG_TYPE_PUSH_GROUP,
            id,
            severity: GL_DEBUG_SEVERITY_NOTIFICATION,
            text: text.clone(),
        };
        let delivery = s.gl.deliver_debug_message(push);
        s.gl.push_debug_group(source_, id, text);
        Some(delivery)
    });
    if let Some(delivery) = delivery {
        dispatch(delivery);
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPushDebugGroup(a: u32, b: u32, c: i32, d: *const c_char) {
    glPushDebugGroupKHR(a, b, c, d)
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPopDebugGroupKHR() {
    let delivery = GlobalState::context(|s| {
        if let Some(m) = s.gl.pop_debug_group() {
            Some(s.gl.deliver_debug_message(m))
        } else {
            None
        }
    });
    if let Some(delivery) = delivery {
        dispatch(delivery);
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glPopDebugGroup() {
    glPopDebugGroupKHR()
}

fn label_object(identifier: u32, name: u32, length: i32, label: *const c_char) {
    let text = if label.is_null() {
        None
    } else {
        match unsafe { input_bytes(label, length, MAX_LABEL_LENGTH_VALUE) } {
            InputBytes::Value(bytes) => Some(bytes),
            InputBytes::Null | InputBytes::TooLong => {
                GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE));
                return;
            }
        }
    };
    GlobalState::context(|s| {
        if !hl_gl::model::context::GlContext::debug_identifier_valid(identifier) {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return;
        }
        if !s.gl.debug_object_valid(identifier, name) {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        s.gl.set_object_label(identifier, name, text);
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectLabelKHR(a: u32, b: u32, c: i32, d: *const c_char) {
    label_object(a, b, c, d)
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectLabel(a: u32, b: u32, c: i32, d: *const c_char) {
    label_object(a, b, c, d)
}

unsafe fn write_label(text: &[u8], size: i32, length: *mut i32, out: *mut c_char) {
    if !length.is_null() {
        let reported = if out.is_null() {
            text.len()
        } else {
            text.len().min(size.saturating_sub(1) as usize)
        };
        unsafe { *length = reported as i32 }
    }
    if size > 0 && !out.is_null() {
        let n = text.len().min(size as usize - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), out.cast(), n);
            *out.add(n) = 0;
        }
    }
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectLabelKHR(
    identifier: u32,
    name: u32,
    size: i32,
    length: *mut i32,
    out: *mut c_char,
) {
    GlobalState::context(|s| {
        if size < 0 {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        if !hl_gl::model::context::GlContext::debug_identifier_valid(identifier) {
            s.gl.set_gl_error(GL_INVALID_ENUM);
            return;
        }
        if !s.gl.debug_object_valid(identifier, name) {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        unsafe { write_label(s.gl.object_label(identifier, name), size, length, out) }
    })
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectLabel(a: u32, b: u32, c: i32, d: *mut i32, e: *mut c_char) {
    glGetObjectLabelKHR(a, b, c, d, e)
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectPtrLabelKHR(ptr: *const c_void, length: i32, label: *const c_char) {
    let text = if label.is_null() {
        None
    } else {
        match unsafe { input_bytes(label, length, MAX_LABEL_LENGTH_VALUE) } {
            InputBytes::Value(bytes) => Some(bytes),
            InputBytes::Null | InputBytes::TooLong => {
                GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_VALUE));
                return;
            }
        }
    };
    GlobalState::context(|s| {
        if !s.gl.has_sync(ptr as usize) {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        s.gl.set_pointer_label(ptr as usize, text);
    });
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glObjectPtrLabel(a: *const c_void, b: i32, c: *const c_char) {
    glObjectPtrLabelKHR(a, b, c)
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectPtrLabelKHR(
    ptr: *const c_void,
    size: i32,
    length: *mut i32,
    out: *mut c_char,
) {
    GlobalState::context(|s| {
        if !s.gl.has_sync(ptr as usize) || size < 0 {
            s.gl.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        unsafe { write_label(s.gl.pointer_label(ptr as usize), size, length, out) }
    })
}
#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glGetObjectPtrLabel(a: *const c_void, b: i32, c: *mut i32, d: *mut c_char) {
    glGetObjectPtrLabelKHR(a, b, c, d)
}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glReleaseShaderCompiler() {}

#[cfg_attr(gles_client, no_mangle)]
pub extern "C" fn glShaderBinary(
    _count: i32,
    _shaders: *const u32,
    _binary_format: u32,
    _binary: *const c_void,
    _length: i32,
) {
    GlobalState::context(|s| s.gl.set_gl_error(GL_INVALID_ENUM));
}
