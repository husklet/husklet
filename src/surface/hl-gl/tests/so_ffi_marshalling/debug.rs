use super::*;

const GL_DEBUG_OUTPUT: u32 = 0x92E0;
const GL_DEBUG_SOURCE_APPLICATION: u32 = 0x824A;
const GL_DEBUG_TYPE_MARKER: u32 = 0x8268;
const GL_DEBUG_SEVERITY_HIGH: u32 = 0x9146;
const GL_DEBUG_CALLBACK_FUNCTION: u32 = 0x8244;
const GL_DEBUG_CALLBACK_USER_PARAM: u32 = 0x8245;
const GL_BUFFER_OBJECT: u32 = 0x82E0;

static CALLBACK_BYTES: Mutex<Option<(Vec<u8>, usize)>> = Mutex::new(None);

unsafe extern "C" fn callback(
    _source: u32,
    _type: u32,
    _id: u32,
    _severity: u32,
    length: i32,
    message: *const c_char,
    user: *const c_void,
) {
    let bytes = unsafe { std::slice::from_raw_parts(message.cast::<u8>(), length as usize) };
    assert_eq!(unsafe { *message.cast::<u8>().add(length as usize) }, 0);
    *CALLBACK_BYTES.lock().unwrap() = Some((bytes.to_vec(), user as usize));
}

#[test]
fn khr_debug_round_trips_through_dlopened_shim_abi() {
    let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(shim) = load() else { return };

    let enable = f!(shim.gles, "glEnable", extern "C" fn(u32));
    let callback_fn = f!(
        shim.gles,
        "glDebugMessageCallbackKHR",
        extern "C" fn(*mut c_void, *const c_void)
    );
    let control = f!(
        shim.gles,
        "glDebugMessageControlKHR",
        extern "C" fn(u32, u32, u32, i32, *const u32, u8)
    );
    let insert = f!(
        shim.gles,
        "glDebugMessageInsertKHR",
        extern "C" fn(u32, u32, u32, u32, i32, *const c_char)
    );
    let get_pointer = f!(
        shim.gles,
        "glGetPointervKHR",
        extern "C" fn(u32, *mut *mut c_void)
    );
    let get_log = f!(
        shim.gles,
        "glGetDebugMessageLogKHR",
        extern "C" fn(
            u32,
            i32,
            *mut u32,
            *mut u32,
            *mut u32,
            *mut u32,
            *mut i32,
            *mut c_char,
        ) -> u32
    );
    let gen_buffers = f!(shim.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let object_label = f!(
        shim.gles,
        "glObjectLabelKHR",
        extern "C" fn(u32, u32, i32, *const c_char)
    );
    let get_object_label = f!(
        shim.gles,
        "glGetObjectLabelKHR",
        extern "C" fn(u32, u32, i32, *mut i32, *mut c_char)
    );

    enable(GL_DEBUG_OUTPUT);
    let user = 0x5a5ausize as *const c_void;
    callback_fn(callback as *mut c_void, user);
    let mut pointer = core::ptr::null_mut();
    get_pointer(GL_DEBUG_CALLBACK_FUNCTION, &mut pointer);
    assert_eq!(pointer, callback as *mut c_void);
    get_pointer(GL_DEBUG_CALLBACK_USER_PARAM, &mut pointer);
    assert_eq!(pointer, user as *mut c_void);

    let bytes = [0xff, 0, 0x80];
    let id = 71;
    control(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        0x1100,
        1,
        &id,
        0,
    );
    insert(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        id,
        GL_DEBUG_SEVERITY_HIGH,
        bytes.len() as i32,
        bytes.as_ptr().cast(),
    );
    assert!(CALLBACK_BYTES.lock().unwrap().is_none());
    control(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        0x1100,
        1,
        &id,
        1,
    );
    insert(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        id,
        GL_DEBUG_SEVERITY_HIGH,
        bytes.len() as i32,
        bytes.as_ptr().cast(),
    );
    assert_eq!(
        CALLBACK_BYTES.lock().unwrap().take(),
        Some((bytes.to_vec(), 0x5a5a))
    );

    callback_fn(core::ptr::null_mut(), core::ptr::null());
    insert(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        72,
        GL_DEBUG_SEVERITY_HIGH,
        bytes.len() as i32,
        bytes.as_ptr().cast(),
    );
    let mut output = [0u8; 8];
    let mut length = 0;
    assert_eq!(
        get_log(
            1,
            output.len() as i32,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            &mut length,
            output.as_mut_ptr().cast(),
        ),
        1
    );
    assert_eq!(length, bytes.len() as i32 + 1);
    assert_eq!(&output[..bytes.len()], &bytes);
    assert_eq!(output[bytes.len()], 0);

    let mut buffer = 0;
    gen_buffers(1, &mut buffer);
    object_label(
        GL_BUFFER_OBJECT,
        buffer,
        bytes.len() as i32,
        bytes.as_ptr().cast(),
    );
    length = -1;
    get_object_label(
        GL_BUFFER_OBJECT,
        buffer,
        0,
        &mut length,
        core::ptr::null_mut(),
    );
    assert_eq!(length, bytes.len() as i32);
    get_object_label(
        GL_BUFFER_OBJECT,
        buffer,
        output.len() as i32,
        &mut length,
        output.as_mut_ptr().cast(),
    );
    assert_eq!(&output[..bytes.len()], &bytes);
}
