use super::*;

fn bind_debug_context() {
    let display = initialized_display();
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_CONTEXT_FLAGS_KHR,
        EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR,
        EGL_NONE,
    ];
    let context = eglCreateContext(
        display,
        CONFIG_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert!(!context.is_null());
    let surface = WindowSurface::create(core::ptr::null_mut());
    assert_eq!(eglMakeCurrent(display, surface, surface, context), EGL_TRUE);
    while glGetError() != GL_NO_ERROR {}
}

fn bind_context_version(major: i32, minor: i32) {
    let display = initialized_display();
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        major,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        minor,
        EGL_NONE,
    ];
    let context = eglCreateContext(
        display,
        CONFIG_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert!(!context.is_null(), "create ES {major}.{minor}");
    let surface = WindowSurface::create(core::ptr::null_mut());
    assert_eq!(eglMakeCurrent(display, surface, surface, context), EGL_TRUE);
    while glGetError() != GL_NO_ERROR {}
}

#[derive(Debug, PartialEq, Eq)]
struct CallbackRecord {
    source: u32,
    type_: u32,
    id: u32,
    severity: u32,
    message: Vec<u8>,
    user: usize,
}

static DEBUG_CALLBACK_RECORD: std::sync::Mutex<Option<CallbackRecord>> =
    std::sync::Mutex::new(None);

unsafe extern "C" fn record_debug_callback(
    source: u32,
    type_: u32,
    id: u32,
    severity: u32,
    length: i32,
    message: *const c_char,
    user: *const c_void,
) {
    let bytes = unsafe { std::slice::from_raw_parts(message.cast::<u8>(), length as usize) };
    *DEBUG_CALLBACK_RECORD.lock().unwrap() = Some(CallbackRecord {
        source,
        type_,
        id,
        severity,
        message: bytes.to_vec(),
        user: user as usize,
    });
}

#[test]
fn khr_debug_callback_pointer_and_application_message_are_exact() {
    bind_debug_context();

    let mut flags = 0;
    glGetIntegerv(GL_CONTEXT_FLAGS, &mut flags);
    assert_eq!(flags as u32, GL_CONTEXT_FLAG_DEBUG_BIT);
    assert_ne!(glIsEnabled(GL_DEBUG_OUTPUT), 0);
    for (pname, expected) in [
        (GL_MAX_DEBUG_MESSAGE_LENGTH, 1024),
        (GL_MAX_DEBUG_LOGGED_MESSAGES, 64),
        (GL_MAX_DEBUG_GROUP_STACK_DEPTH, 64),
        (GL_MAX_LABEL_LENGTH, 256),
    ] {
        let mut value = 0;
        glGetIntegerv(pname, &mut value);
        assert_eq!(value, expected, "{pname:#x}");
    }

    let user = 0x1234usize as *const c_void;
    glDebugMessageCallbackKHR(record_debug_callback as *mut c_void, user);
    let mut callback = core::ptr::null_mut();
    let mut reported_user = core::ptr::null_mut();
    glGetPointervKHR(GL_DEBUG_CALLBACK_FUNCTION, &mut callback);
    glGetPointervKHR(GL_DEBUG_CALLBACK_USER_PARAM, &mut reported_user);
    assert_eq!(callback, record_debug_callback as *mut c_void);
    assert_eq!(reported_user, user as *mut c_void);

    let message = b"application marker";
    glDebugMessageInsertKHR(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        77,
        GL_DEBUG_SEVERITY_HIGH,
        message.len() as i32,
        message.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_NO_ERROR);
    assert_eq!(
        DEBUG_CALLBACK_RECORD.lock().unwrap().take(),
        Some(CallbackRecord {
            source: GL_DEBUG_SOURCE_APPLICATION,
            type_: GL_DEBUG_TYPE_MARKER,
            id: 77,
            severity: GL_DEBUG_SEVERITY_HIGH,
            message: b"application marker".to_vec(),
            user: 0x1234,
        })
    );

    glGetPointervKHR(GL_TEXTURE, &mut callback);
    assert_eq!(glGetError(), GL_INVALID_ENUM);

    glDebugMessageInsertKHR(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_PUSH_GROUP,
        78,
        GL_DEBUG_SEVERITY_HIGH,
        message.len() as i32,
        message.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_INVALID_ENUM);
}

#[test]
fn khr_debug_log_groups_filters_and_label_lifetime_are_observable() {
    bind_debug_context();
    glDebugMessageCallbackKHR(core::ptr::null_mut(), core::ptr::null());

    let message = [0xff, 0, 0x80];
    glDebugMessageInsertKHR(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        9,
        GL_DEBUG_SEVERITY_HIGH,
        message.len() as i32,
        message.as_ptr().cast(),
    );
    let mut logged = 0;
    let mut next = 0;
    glGetIntegerv(GL_DEBUG_LOGGED_MESSAGES, &mut logged);
    glGetIntegerv(GL_DEBUG_NEXT_LOGGED_MESSAGE_LENGTH, &mut next);
    assert_eq!(logged, 1);
    assert_eq!(next, message.len() as i32 + 1);

    let (mut source, mut type_, mut id, mut severity, mut length) = (0, 0, 0, 0, 0);
    let mut output = [0u8; 32];
    assert_eq!(
        glGetDebugMessageLogKHR(
            1,
            output.len() as i32,
            &mut source,
            &mut type_,
            &mut id,
            &mut severity,
            &mut length,
            output.as_mut_ptr(),
        ),
        1
    );
    assert_eq!(
        (source, type_, id, severity, length),
        (
            GL_DEBUG_SOURCE_APPLICATION,
            GL_DEBUG_TYPE_MARKER,
            9,
            GL_DEBUG_SEVERITY_HIGH,
            message.len() as i32 + 1,
        )
    );
    assert_eq!(&output[..message.len()], &message);

    glDebugMessageInsertKHR(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        10,
        GL_DEBUG_SEVERITY_HIGH,
        message.len() as i32,
        message.as_ptr().cast(),
    );
    assert_eq!(
        glGetDebugMessageLogKHR(
            1,
            -1,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            &mut id,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        ),
        1,
        "a null messageLog ignores even a negative bufSize"
    );
    assert_eq!(id, 10);

    let group = b"filtered group";
    glPushDebugGroupKHR(
        GL_DEBUG_SOURCE_APPLICATION,
        11,
        group.len() as i32,
        group.as_ptr().cast(),
    );
    glDebugMessageControlKHR(
        GL_DONT_CARE,
        GL_DONT_CARE,
        GL_DONT_CARE,
        0,
        core::ptr::null(),
        0,
    );
    glDebugMessageInsertKHR(
        GL_DEBUG_SOURCE_APPLICATION,
        GL_DEBUG_TYPE_MARKER,
        12,
        GL_DEBUG_SEVERITY_HIGH,
        message.len() as i32,
        message.as_ptr().cast(),
    );
    glPopDebugGroupKHR();
    glGetIntegerv(GL_DEBUG_GROUP_STACK_DEPTH, &mut logged);
    assert_eq!(logged, 1);

    let mut buffer = 0;
    glGenBuffers(1, &mut buffer);
    glBindBuffer(GL_ARRAY_BUFFER, buffer);
    let label = b"vertex bytes";
    glObjectLabelKHR(
        GL_BUFFER_OBJECT,
        buffer,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    let mut label_length = 0;
    let mut label_output = [0u8; 32];
    glGetObjectLabelKHR(
        GL_BUFFER_OBJECT,
        buffer,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(label_length, label.len() as i32);
    label_length = -1;
    glGetObjectLabelKHR(
        GL_BUFFER_OBJECT,
        buffer,
        0,
        &mut label_length,
        core::ptr::null_mut(),
    );
    assert_eq!(label_length, label.len() as i32);

    glObjectLabelKHR(
        GL_TEXTURE,
        buffer,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_INVALID_VALUE, "wrong object namespace");
    glObjectLabelKHR(0xdead, buffer, label.len() as i32, label.as_ptr().cast());
    assert_eq!(glGetError(), GL_INVALID_ENUM, "unknown object namespace");
    let overlong = [b'x'; 256];
    glObjectLabelKHR(
        GL_BUFFER_OBJECT,
        buffer,
        overlong.len() as i32,
        overlong.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_INVALID_VALUE, "label limit is exclusive");
    glDeleteBuffers(1, &buffer);
    glGetObjectLabelKHR(
        GL_BUFFER_OBJECT,
        buffer,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(glGetError(), GL_INVALID_VALUE);

    let mut sink = hl_gpu::RecordingSink::with_full_caps();
    let sync = GlobalState::context(|state| {
        sync::fence_sync(&mut state.gl, &mut sink, GL_SYNC_GPU_COMMANDS_COMPLETE, 0)
            .expect("recording sink accepts glFenceSync") as *mut c_void
    });
    glObjectPtrLabelKHR(sync, label.len() as i32, label.as_ptr().cast());
    label_length = 0;
    glGetObjectPtrLabelKHR(
        sync,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(label_length, label.len() as i32);
    glDeleteSync(sync);
    glGetObjectPtrLabelKHR(
        sync,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(glGetError(), GL_INVALID_VALUE);

    let shader = glCreateShader(GL_VERTEX_SHADER);
    let program = glCreateProgram();
    glAttachShader(program, shader);
    glObjectLabelKHR(
        GL_SHADER_OBJECT,
        shader,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    glDeleteShader(shader);
    glGetObjectLabelKHR(
        GL_SHADER_OBJECT,
        shader,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(
        glGetError(),
        GL_NO_ERROR,
        "delete-pending shader stays live"
    );
    glDetachShader(program, shader);
    glGetObjectLabelKHR(
        GL_SHADER_OBJECT,
        shader,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(
        glGetError(),
        GL_INVALID_VALUE,
        "actual destruction clears label"
    );

    let vertex = glCreateShader(GL_VERTEX_SHADER);
    let fragment = glCreateShader(GL_FRAGMENT_SHADER);
    let vertex_source =
        std::ffi::CString::new("#version 300 es\nvoid main(){gl_Position=vec4(0.0);}").unwrap();
    let fragment_source = std::ffi::CString::new(
        "#version 300 es\nprecision mediump float; out vec4 color; void main(){color=vec4(1.0);}",
    )
    .unwrap();
    let vertex_pointer = vertex_source.as_ptr();
    let fragment_pointer = fragment_source.as_ptr();
    glShaderSource(vertex, 1, &vertex_pointer, core::ptr::null());
    glShaderSource(fragment, 1, &fragment_pointer, core::ptr::null());
    glCompileShader(vertex);
    glCompileShader(fragment);
    let pending_program = glCreateProgram();
    glAttachShader(pending_program, vertex);
    glAttachShader(pending_program, fragment);
    glLinkProgram(pending_program);
    glUseProgram(pending_program);
    assert_eq!(glGetError(), GL_NO_ERROR);
    glObjectLabelKHR(
        GL_PROGRAM_OBJECT,
        pending_program,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    glDeleteProgram(pending_program);
    glGetObjectLabelKHR(
        GL_PROGRAM_OBJECT,
        pending_program,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(
        glGetError(),
        GL_NO_ERROR,
        "current delete-pending program is live"
    );
    glUseProgram(0);
    glGetObjectLabelKHR(
        GL_PROGRAM_OBJECT,
        pending_program,
        label_output.len() as i32,
        &mut label_length,
        label_output.as_mut_ptr(),
    );
    assert_eq!(
        glGetError(),
        GL_INVALID_VALUE,
        "unbinding destroys pending program"
    );
}

#[test]
fn khr_debug_local_and_share_group_label_namespaces_do_not_leak() {
    let display = initialized_display();
    let attributes = [EGL_CONTEXT_CLIENT_VERSION, 3, EGL_NONE];
    let first = eglCreateContext(
        display,
        CONFIG_TOKEN as *mut c_void,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    let second = eglCreateContext(
        display,
        CONFIG_TOKEN as *mut c_void,
        first,
        attributes.as_ptr(),
    );
    let surface = WindowSurface::create(core::ptr::null_mut());
    assert_eq!(eglMakeCurrent(display, surface, surface, first), EGL_TRUE);

    let mut framebuffer = 0;
    let mut buffer = 0;
    glGenFramebuffers(1, &mut framebuffer);
    glGenBuffers(1, &mut buffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glBindBuffer(GL_ARRAY_BUFFER, buffer);
    glObjectLabelKHR(GL_FRAMEBUFFER, framebuffer, 5, b"first".as_ptr().cast());
    glObjectLabelKHR(GL_BUFFER_OBJECT, buffer, 6, b"shared".as_ptr().cast());

    assert_eq!(eglMakeCurrent(display, surface, surface, second), EGL_TRUE);
    let mut second_framebuffer = 0;
    glGenFramebuffers(1, &mut second_framebuffer);
    assert_eq!(second_framebuffer, framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, second_framebuffer);
    glObjectLabelKHR(
        GL_FRAMEBUFFER,
        second_framebuffer,
        6,
        b"second".as_ptr().cast(),
    );

    let mut output = [0u8; 16];
    let mut length = 0;
    glGetObjectLabelKHR(
        GL_BUFFER_OBJECT,
        buffer,
        output.len() as i32,
        &mut length,
        output.as_mut_ptr(),
    );
    assert_eq!(&output[..length as usize], b"shared");
    glGetObjectLabelKHR(
        GL_FRAMEBUFFER,
        second_framebuffer,
        output.len() as i32,
        &mut length,
        output.as_mut_ptr(),
    );
    assert_eq!(&output[..length as usize], b"second");

    assert_eq!(eglMakeCurrent(display, surface, surface, first), EGL_TRUE);
    glGetObjectLabelKHR(
        GL_FRAMEBUFFER,
        framebuffer,
        output.len() as i32,
        &mut length,
        output.as_mut_ptr(),
    );
    assert_eq!(&output[..length as usize], b"first");
}

#[test]
fn khr_debug_rejects_reserved_names_until_object_creation() {
    bind_debug_context();
    let label = b"materialized";
    let mut names = [0u32; 7];
    glGenBuffers(1, &mut names[0]);
    glGenTextures(1, &mut names[1]);
    glGenFramebuffers(1, &mut names[2]);
    glGenVertexArrays(1, &mut names[3]);
    glGenQueries(1, &mut names[4]);
    glGenProgramPipelines(1, &mut names[5]);
    glGenTransformFeedbacks(1, &mut names[6]);
    let identifiers = [
        GL_BUFFER_OBJECT,
        GL_TEXTURE,
        GL_FRAMEBUFFER,
        GL_VERTEX_ARRAY_OBJECT,
        GL_QUERY_OBJECT,
        GL_PROGRAM_PIPELINE_OBJECT,
        GL_TRANSFORM_FEEDBACK,
    ];
    for (identifier, name) in identifiers.into_iter().zip(names) {
        glObjectLabelKHR(identifier, name, label.len() as i32, label.as_ptr().cast());
        assert_eq!(glGetError(), GL_INVALID_VALUE, "{identifier:#x}");
        let mut length = 0;
        let mut output = [0u8; 16];
        glGetObjectLabelKHR(
            identifier,
            name,
            output.len() as i32,
            &mut length,
            output.as_mut_ptr(),
        );
        assert_eq!(glGetError(), GL_INVALID_VALUE, "get {identifier:#x}");
    }

    glBindBuffer(0xdead, names[0]);
    assert_eq!(glGetError(), GL_INVALID_ENUM);
    glObjectLabelKHR(
        GL_BUFFER_OBJECT,
        names[0],
        label.len() as i32,
        label.as_ptr().cast(),
    );
    assert_eq!(
        glGetError(),
        GL_INVALID_VALUE,
        "invalid-target bind must not create the buffer"
    );

    glBindBuffer(GL_ARRAY_BUFFER, names[0]);
    glBindTexture(GL_TEXTURE_2D, names[1]);
    glBindFramebuffer(GL_FRAMEBUFFER, names[2]);
    glBindVertexArray(names[3]);
    glBeginQuery(GL_ANY_SAMPLES_PASSED, names[4]);
    glEndQuery(GL_ANY_SAMPLES_PASSED);
    glBindProgramPipeline(names[5]);
    glBindTransformFeedback(GL_TRANSFORM_FEEDBACK, names[6]);
    for (identifier, name) in identifiers.into_iter().zip(names) {
        glObjectLabelKHR(identifier, name, label.len() as i32, label.as_ptr().cast());
        assert_eq!(glGetError(), GL_NO_ERROR, "{identifier:#x}");
    }
    let mut sampler = 0;
    glGenSamplers(1, &mut sampler);
    glObjectLabelKHR(
        GL_SAMPLER_OBJECT,
        sampler,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    assert_eq!(glGetError(), GL_NO_ERROR, "samplers are created by Gen");

    let mut stage_pipeline = 0;
    let mut active_pipeline = 0;
    glGenProgramPipelines(1, &mut stage_pipeline);
    glGenProgramPipelines(1, &mut active_pipeline);
    for pipeline in [stage_pipeline, active_pipeline] {
        glObjectLabelKHR(
            GL_PROGRAM_PIPELINE_OBJECT,
            pipeline,
            label.len() as i32,
            label.as_ptr().cast(),
        );
        assert_eq!(glGetError(), GL_INVALID_VALUE, "Gen reserves pipeline");
    }
    glUseProgramStages(stage_pipeline, GL_VERTEX_SHADER_BIT, 0);
    glObjectLabelKHR(
        GL_PROGRAM_PIPELINE_OBJECT,
        stage_pipeline,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    assert_eq!(
        glGetError(),
        GL_NO_ERROR,
        "UseProgramStages creates pipeline"
    );
    glActiveShaderProgram(active_pipeline, 0);
    glObjectLabelKHR(
        GL_PROGRAM_PIPELINE_OBJECT,
        active_pipeline,
        label.len() as i32,
        label.as_ptr().cast(),
    );
    assert_eq!(
        glGetError(),
        GL_NO_ERROR,
        "ActiveShaderProgram creates pipeline"
    );
}

#[test]
fn buffer_targets_materialize_only_in_their_client_version() {
    let label = b"buffer";
    for (major, minor, rejected) in [(2, 0, GL_UNIFORM_BUFFER), (3, 0, GL_SHADER_STORAGE_BUFFER)] {
        bind_context_version(major, minor);
        let mut buffer = 0;
        glGenBuffers(1, &mut buffer);
        glBindBuffer(rejected, buffer);
        assert_eq!(glGetError(), GL_INVALID_ENUM, "ES {major}.{minor}");
        glObjectLabelKHR(
            GL_BUFFER_OBJECT,
            buffer,
            label.len() as i32,
            label.as_ptr().cast(),
        );
        assert_eq!(
            glGetError(),
            GL_INVALID_VALUE,
            "rejected ES {major}.{minor} target must not create the buffer"
        );
    }

    bind_context_version(3, 1);
    for target in [
        GL_ATOMIC_COUNTER_BUFFER,
        GL_DISPATCH_INDIRECT_BUFFER,
        GL_DRAW_INDIRECT_BUFFER,
        GL_SHADER_STORAGE_BUFFER,
    ] {
        let mut buffer = 0;
        glGenBuffers(1, &mut buffer);
        glBindBuffer(target, buffer);
        assert_eq!(glGetError(), GL_NO_ERROR, "ES 3.1 target {target:#x}");
        glObjectLabelKHR(
            GL_BUFFER_OBJECT,
            buffer,
            label.len() as i32,
            label.as_ptr().cast(),
        );
        assert_eq!(glGetError(), GL_NO_ERROR, "materialized {target:#x}");
    }
}

#[test]
fn readback_writer_preserves_pack_skips_and_row_gaps() {
    let store = hl_gl::model::context::PixelStore {
        pack_alignment: 4,
        pack_row_length: 4,
        pack_skip_rows: 1,
        pack_skip_pixels: 1,
        ..Default::default()
    };
    let layout = store.pack_layout(2, 2, 4).unwrap();
    let pixels = [
        1, 2, 3, 4, 5, 6, 7, 8, // row 0
        9, 10, 11, 12, 13, 14, 15, 16, // row 1
    ];
    let mut destination = vec![0xA5; layout.required_size()];

    unsafe {
        write_packed_rows(
            &pixels,
            layout,
            2,
            destination.as_mut_ptr().cast::<c_void>(),
        )
    };

    assert!(destination[..20].iter().all(|&byte| byte == 0xA5));
    assert_eq!(&destination[20..28], &pixels[..8]);
    assert!(destination[28..36].iter().all(|&byte| byte == 0xA5));
    assert_eq!(&destination[36..44], &pixels[8..]);
}

#[test]
fn matrix_marshalling_leaves_std140_padding_to_the_uniform_model() {
    let values = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    let packed = unsafe { mat_bytes_cr(3, 3, 1, false, values.as_ptr()) };
    assert_eq!(packed.len(), 9 * std::mem::size_of::<f32>());

    let (uniforms, size) = hl_gl::adapter::glsl::StageSources::new(
        "uniform mat3 transform;\nvoid main(){gl_Position=vec4(0);}",
        "void main(){gl_FragColor=vec4(1);}",
    )
    .uniform_layout()
    .expect("mat3 layout");
    let mut block = vec![0; size as usize];
    uniforms[0].write(&mut block, &packed);

    for (offset, expected) in [
        (0, 1.0f32),
        (4, 2.0),
        (8, 3.0),
        (16, 4.0),
        (20, 5.0),
        (24, 6.0),
        (32, 7.0),
        (36, 8.0),
        (40, 9.0),
    ] {
        assert_eq!(
            f32::from_le_bytes(block[offset..offset + 4].try_into().unwrap()),
            expected
        );
    }
}

// `EGL_KHR_create_context` defines only the debug flag for OpenGL ES. Robust access uses the separately
// advertised EXT attribute; the KHR robust and forward-compatible flag bits are OpenGL-only.
#[test]
fn khr_create_context_flags_accept_debug_and_reject_opengl_only_bits() {
    let display = initialized_display();
    let config = CONFIG_TOKEN as *mut c_void;
    let with_flags = |flags: i32| {
        [
            EGL_CONTEXT_CLIENT_VERSION,
            3,
            EGL_CONTEXT_MINOR_VERSION_KHR,
            0,
            EGL_CONTEXT_FLAGS_KHR,
            flags,
            EGL_NONE,
        ]
    };

    for flags in [0, EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR] {
        let attributes = with_flags(flags);
        let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
        assert!(
            !context.is_null(),
            "EGL_CONTEXT_FLAGS_KHR = {flags} accepted"
        );
        assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
    }

    for bit in [
        EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR,
        EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR,
    ] {
        let attributes = with_flags(bit);
        assert!(
            eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr()).is_null()
        );
        assert_eq!(eglGetError(), EGL_BAD_ATTRIBUTE);
    }

    // `EGL_KHR_create_context`'s own spelling of the reset-notification attribute is accepted too.
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR,
        EGL_LOSE_CONTEXT_ON_RESET_EXT,
        EGL_NONE,
    ];
    let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
    assert!(!context.is_null());
    let mut value = 0;
    assert_eq!(
        eglQueryContext(
            display,
            context,
            EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
            &mut value
        ),
        EGL_TRUE
    );
    assert_eq!(value, EGL_LOSE_CONTEXT_ON_RESET_EXT);
    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

#[test]
fn robust_es31_context_is_validated_and_queryable() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT,
        EGL_TRUE as i32,
        EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
        EGL_LOSE_CONTEXT_ON_RESET_EXT,
        EGL_NONE,
    ];
    let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
    assert!(!context.is_null());

    let mut value = 0;
    assert_eq!(
        eglQueryContext(
            display,
            context,
            EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT,
            &mut value
        ),
        EGL_TRUE
    );
    assert_eq!(value, EGL_TRUE as i32);
    assert_eq!(
        eglQueryContext(display, context, EGL_CONTEXT_MINOR_VERSION_KHR, &mut value),
        EGL_TRUE
    );
    assert_eq!(value, 1);
    assert_eq!(
        eglQueryContext(
            display,
            context,
            EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
            &mut value
        ),
        EGL_TRUE
    );
    assert_eq!(value, EGL_LOSE_CONTEXT_ON_RESET_EXT);

    let bad_attributes = [0xDEAD, 1, EGL_NONE];
    assert!(eglCreateContext(
        display,
        config,
        core::ptr::null_mut(),
        bad_attributes.as_ptr()
    )
    .is_null());
    assert_eq!(eglGetError(), EGL_BAD_ATTRIBUTE);

    let unsupported_version = [EGL_CONTEXT_CLIENT_VERSION, 1, EGL_NONE];
    assert!(eglCreateContext(
        display,
        config,
        core::ptr::null_mut(),
        unsupported_version.as_ptr()
    )
    .is_null());
    assert_eq!(eglGetError(), EGL_BAD_MATCH);

    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

#[test]
fn chrome_es30_es20_and_dawn_es31_requests_report_the_selected_profile() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    for (major, minor, version, glsl) in [
        (3, 0, "OpenGL ES 3.0 hl-gl", "OpenGL ES GLSL ES 3.00"),
        (2, 0, "OpenGL ES 2.0 hl-gl", "OpenGL ES GLSL ES 1.00"),
        (3, 1, "OpenGL ES 3.1 hl-gl", "OpenGL ES GLSL ES 3.10"),
    ] {
        let attributes = [
            EGL_CONTEXT_CLIENT_VERSION,
            major,
            EGL_CONTEXT_MINOR_VERSION_KHR,
            minor,
            EGL_NONE,
        ];
        let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
        assert!(!context.is_null(), "ES {major}.{minor} context");
        assert_eq!(
            eglMakeCurrent(
                display,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                context
            ),
            EGL_TRUE
        );
        let mut reported_major = 0;
        let mut reported_minor = 0;
        glGetIntegerv(GL_MAJOR_VERSION, &mut reported_major);
        glGetIntegerv(GL_MINOR_VERSION, &mut reported_minor);
        assert_eq!((reported_major, reported_minor), (major, minor));
        let version_ptr = glGetString(GL_VERSION) as *const c_char;
        let glsl_ptr = glGetString(GL_SHADING_LANGUAGE_VERSION) as *const c_char;
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(version_ptr) }.to_str(),
            Ok(version)
        );
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(glsl_ptr) }.to_str(),
            Ok(glsl)
        );
        assert_eq!(
            eglMakeCurrent(
                display,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut()
            ),
            EGL_TRUE
        );
        assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
    }
}

#[test]
fn chrome_shared_no_error_context_has_context_local_error_semantics() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    let regular_attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        0,
        EGL_NONE,
    ];
    let regular = eglCreateContext(
        display,
        config,
        core::ptr::null_mut(),
        regular_attributes.as_ptr(),
    );
    assert!(!regular.is_null());

    let no_error_attributes = [
        EGL_CONTEXT_OPENGL_NO_ERROR_KHR,
        EGL_TRUE as i32,
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        0,
        EGL_NONE,
    ];
    let shared = eglCreateContext(display, config, regular, no_error_attributes.as_ptr());
    assert!(!shared.is_null());

    let mut no_error = 0;
    assert_eq!(
        eglQueryContext(
            display,
            shared,
            EGL_CONTEXT_OPENGL_NO_ERROR_KHR,
            &mut no_error
        ),
        EGL_TRUE
    );
    assert_eq!(no_error, EGL_TRUE as i32);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            shared
        ),
        EGL_TRUE
    );
    glEGLImageTargetTexture2DOES(0xDEAD, core::ptr::null_mut());
    assert_eq!(glGetError(), GL_NO_ERROR);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            regular
        ),
        EGL_TRUE
    );
    glEGLImageTargetTexture2DOES(0xDEAD, core::ptr::null_mut());
    assert_eq!(glGetError(), GL_INVALID_ENUM);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert_eq!(eglDestroyContext(display, shared), EGL_TRUE);
    assert_eq!(eglDestroyContext(display, regular), EGL_TRUE);
}

#[test]
fn dawn_required_egl_procedures_all_resolve() {
    for name in [
        "eglBindAPI",
        "eglChooseConfig",
        "eglCreateContext",
        "eglCreatePbufferSurface",
        "eglDestroyContext",
        "eglDestroySurface",
        "eglGetConfigAttrib",
        "eglGetCurrentContext",
        "eglGetCurrentDisplay",
        "eglGetCurrentSurface",
        "eglGetDisplay",
        "eglGetError",
        "eglGetProcAddress",
        "eglInitialize",
        "eglMakeCurrent",
        "eglQueryContext",
        "eglQueryString",
        "eglQuerySurface",
        "eglSwapBuffers",
        "eglTerminate",
        "eglWaitClient",
    ] {
        let name = std::ffi::CString::new(name).unwrap();
        assert!(
            !eglGetProcAddress(name.as_ptr()).is_null(),
            "{} must resolve for Dawn",
            name.to_string_lossy()
        );
    }
}

#[path = "tests/current.rs"]
mod current_binding_tests;

#[path = "tests/hostile.rs"]
mod hostile_input_tests;

#[path = "tests/advertised.rs"]
mod advertised_extension_tests;

/// `eglGetDisplay(native_display)` is the ONLY place a wayland app hands the shim its own `wl_display*`,
/// and the app-surface presenter needs that connection: deriving it from the `wl_surface` proxy takes
/// `wl_proxy_get_display`, a Wayland 1.23+ symbol that 24.04-era guests (libwayland 1.22) do not export.
/// Discarding it there is what latched the presenter unavailable and sent every frame through a readback
/// present onto the shim's own mirror window. `EGL_DEFAULT_DISPLAY` is null and must stay legal — Chrome's
/// GPU process passes it — so a null never clears a connection already learnt.
#[test]
fn egl_get_display_keeps_the_app_wl_display_and_a_null_never_clears_it() {
    const APP_DISPLAY: usize = 0xDEAD_D150;
    assert_eq!(
        eglGetDisplay(APP_DISPLAY as *mut c_void),
        DISPLAY_TOKEN as *mut c_void
    );
    assert_eq!(
        GlobalState::access(|s| s.app_display),
        APP_DISPLAY,
        "the app's own wl_display must reach the app-surface presenter"
    );

    // EGL_DEFAULT_DISPLAY: still a valid display, and it must not forget what we already know.
    assert_eq!(
        eglGetDisplay(core::ptr::null_mut()),
        DISPLAY_TOKEN as *mut c_void
    );
    assert_eq!(GlobalState::access(|s| s.app_display), APP_DISPLAY);
}

/// The present-route counters are the guard this driver did not have: `swap` decided the route every
/// frame and discarded it, so a window whose frames had all degraded to a `glReadPixels` readback still
/// presented correct pixels and passed every rung that only judges pixels. `eglQueryString` reports them
/// so a probe can assert the route, and an unrecognized name must still be `EGL_BAD_PARAMETER`.
#[test]
fn present_route_is_queryable_and_counts_the_degraded_readback() {
    let dpy = DISPLAY_TOKEN as *mut c_void;
    let read = || {
        let answer = eglQueryString(dpy, EGL_PRESENT_ROUTE_HL);
        assert!(!answer.is_null(), "the route query must answer");
        unsafe { std::ffi::CStr::from_ptr(answer) }
            .to_str()
            .expect("route is utf-8")
            .to_owned()
    };

    let before = read();
    assert!(
        before.starts_with("native=") && before.contains(" readback="),
        "unexpected route string {before:?}"
    );
    let readback_before = present::PresentStats::counts().1;

    // A degraded window frame must move the readback counter — that is the whole assertion a rung makes.
    present::PresentStats::record_readback(0xABCD);
    assert_eq!(present::PresentStats::counts().1, readback_before + 1);
    assert_eq!(
        read(),
        format!(
            "native={} readback={}",
            present::PresentStats::counts().0,
            readback_before + 1
        )
    );

    // The new name must not have weakened the spec'd rejection of an unknown one.
    assert!(eglQueryString(dpy, 0x1234).is_null());
    assert_eq!(eglGetError(), EGL_BAD_PARAMETER);
}

/// A buffer size is untrusted guest input and must be refused at the boundary, before anything is
/// allocated or marshalled.
///
/// `glBufferData(GL_ARRAY_BUFFER, 1 << 38, NULL, GL_STATIC_DRAW)` — one call — drove the HOST worker to
/// 17.9 GiB RSS, left the machine with 75 MiB free, and on one run killed the workspace's execution
/// domain outright. GLES 3.0 2.9.2 requires `GL_OUT_OF_MEMORY` for a data store the GL cannot create and
/// `GL_INVALID_VALUE` for a negative size; neither was reported and neither bounded the allocation.
#[test]
fn an_absurd_buffer_size_is_refused_instead_of_allocated() {
    use super::objects::BufferRequest;
    const CEILING: u64 = 256 << 20;

    // The reported reproduction: a quarter-terabyte reservation.
    assert_eq!(
        BufferRequest::refusal(1 << 38, CEILING),
        Some(GL_OUT_OF_MEMORY),
        "a size past the negotiated ceiling must be GL_OUT_OF_MEMORY"
    );
    assert_eq!(
        BufferRequest::refusal(isize::MAX, CEILING),
        Some(GL_OUT_OF_MEMORY)
    );
    // Negative sizes are GL_INVALID_VALUE, not out-of-memory and not a crash.
    assert_eq!(BufferRequest::refusal(-1, CEILING), Some(GL_INVALID_VALUE));
    assert_eq!(
        BufferRequest::refusal(isize::MIN, CEILING),
        Some(GL_INVALID_VALUE)
    );
    // The ceiling refuses, it does not forbid: ordinary sizes and the exact ceiling proceed.
    assert_eq!(BufferRequest::refusal(0, CEILING), None);
    assert_eq!(BufferRequest::refusal(64, CEILING), None);
    assert_eq!(BufferRequest::refusal(CEILING as isize, CEILING), None);
    assert_eq!(
        BufferRequest::refusal(CEILING as isize + 1, CEILING),
        Some(GL_OUT_OF_MEMORY)
    );

    // And the entry point itself survives the call rather than aborting the process.
    glBufferData(0x8892, 1 << 38, core::ptr::null(), 0x88E4);
}

/// The reported reproduction, end to end: a failing `eglBindAPI` followed by a succeeding one must leave
/// `EGL_SUCCESS`, and the prologue must apply to EVERY entry point rather than the handful that happened
/// to be audited.
///
/// The error is sticky only until the next command runs — clearing on ENTRY is what makes a succeeding
/// command report success without every entry point having to remember to say so on each of its return
/// paths. `eglGetError` is deliberately exempt: it reads and resets.
#[test]
fn a_succeeding_command_reports_success_across_the_entry_point_surface() {
    const EGL_OPENGL_API: u32 = 0x30A2;
    let dpy = DISPLAY_TOKEN as *mut c_void;

    // The exact sequence from the defect report.
    assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE);
    assert_eq!(eglBindAPI(EGL_OPENGL_ES_API), EGL_TRUE);
    assert_eq!(
        eglGetError(),
        hl_gl::result::EGL_SUCCESS,
        "a succeeding eglBindAPI must clear the previous failure"
    );

    // The same must hold for a representative spread of entry points, not just eglBindAPI: a query, a
    // display getter, a string query and a proc-address lookup.
    for (name, succeed) in [
        ("eglQueryAPI", &(|| eglQueryAPI() != 0) as &dyn Fn() -> bool),
        (
            "eglGetDisplay",
            &(|| !eglGetDisplay(core::ptr::null_mut()).is_null()),
        ),
        (
            "eglQueryString",
            &(|| !eglQueryString(dpy, EGL_VENDOR).is_null()),
        ),
        (
            "eglInitialize",
            &(|| {
                let (mut major, mut minor) = (0, 0);
                eglInitialize(dpy, &mut major, &mut minor) == EGL_TRUE
            }),
        ),
    ] {
        assert_eq!(eglBindAPI(EGL_OPENGL_API), EGL_FALSE, "arm the error");
        assert!(
            succeed(),
            "{name} must succeed for this check to mean anything"
        );
        assert_eq!(
            eglGetError(),
            hl_gl::result::EGL_SUCCESS,
            "{name} succeeded, so it must have cleared the pending error"
        );
    }
}

/// A display handle this driver never issued is `EGL_BAD_DISPLAY`, and with `EGL_NO_DISPLAY` the only
/// legal `eglQueryString` is the CLIENT extension string.
///
/// Both were reported as succeeding, which is worse than failing: a conformance suite took a bogus
/// handle for a working display and proceeded on it, so every later failure landed far from the mistake.
#[test]
fn an_unissued_display_and_a_displayless_query_are_refused() {
    let good = DISPLAY_TOKEN as *mut c_void;
    let bogus = 0xdeadusize as *mut c_void;
    let (mut major, mut minor) = (0, 0);

    assert_eq!(eglInitialize(bogus, &mut major, &mut minor), EGL_FALSE);
    assert_eq!(eglGetError(), EGL_BAD_DISPLAY);
    assert_eq!((major, minor), (0, 0), "a refused init must write nothing");

    // The real display still initializes, and reports EGL 1.4.
    assert_eq!(eglInitialize(good, &mut major, &mut minor), EGL_TRUE);
    assert_eq!((major, minor), (1, 4));
    assert_eq!(eglGetError(), hl_gl::result::EGL_SUCCESS);

    // EGL_NO_DISPLAY: client extensions only.
    assert!(
        !eglQueryString(core::ptr::null_mut(), EGL_EXTENSIONS_Q).is_null(),
        "the client extension string is the one legal displayless query"
    );
    assert_eq!(eglGetError(), hl_gl::result::EGL_SUCCESS);
    for name in [EGL_VENDOR, EGL_VERSION_Q, EGL_CLIENT_APIS] {
        assert!(
            eglQueryString(core::ptr::null_mut(), name).is_null(),
            "{name:#x} is per-display and must not answer without one"
        );
        assert_eq!(eglGetError(), EGL_BAD_DISPLAY, "{name:#x}");
    }
    // With a real display they all still answer.
    for name in [EGL_VENDOR, EGL_VERSION_Q, EGL_CLIENT_APIS, EGL_EXTENSIONS_Q] {
        assert!(!eglQueryString(good, name).is_null(), "{name:#x}");
    }
}
