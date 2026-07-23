use super::*;

#[test]
fn gl_program_reflection_marshals_real_values() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_create_shader = f!(sh.gles, "glCreateShader", extern "C" fn(u32) -> u32);
    let gl_shader_source = f!(
        sh.gles,
        "glShaderSource",
        extern "C" fn(u32, i32, *const *const c_char, *const i32)
    );
    let gl_compile_shader = f!(sh.gles, "glCompileShader", extern "C" fn(u32));
    let gl_create_program = f!(sh.gles, "glCreateProgram", extern "C" fn() -> u32);
    let gl_attach_shader = f!(sh.gles, "glAttachShader", extern "C" fn(u32, u32));
    let gl_link_program = f!(sh.gles, "glLinkProgram", extern "C" fn(u32));
    let gl_use_program = f!(sh.gles, "glUseProgram", extern "C" fn(u32));
    let gl_get_shaderiv = f!(sh.gles, "glGetShaderiv", extern "C" fn(u32, u32, *mut i32));
    let gl_get_programiv = f!(sh.gles, "glGetProgramiv", extern "C" fn(u32, u32, *mut i32));
    let gl_get_uniform_location = f!(
        sh.gles,
        "glGetUniformLocation",
        extern "C" fn(u32, *const c_char) -> i32
    );
    let gl_get_attrib_location = f!(
        sh.gles,
        "glGetAttribLocation",
        extern "C" fn(u32, *const c_char) -> i32
    );
    let gl_get_active_uniform = f!(
        sh.gles,
        "glGetActiveUniform",
        extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char)
    );
    let gl_get_active_attrib = f!(
        sh.gles,
        "glGetActiveAttrib",
        extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char)
    );
    let gl_get_attached_shaders = f!(
        sh.gles,
        "glGetAttachedShaders",
        extern "C" fn(u32, i32, *mut i32, *mut u32)
    );
    let gl_get_program_info_log = f!(
        sh.gles,
        "glGetProgramInfoLog",
        extern "C" fn(u32, i32, *mut i32, *mut c_char)
    );
    let gl_get_shader_info_log = f!(
        sh.gles,
        "glGetShaderInfoLog",
        extern "C" fn(u32, i32, *mut i32, *mut c_char)
    );
    let gl_uniform4f = f!(
        sh.gles,
        "glUniform4f",
        extern "C" fn(i32, f32, f32, f32, f32)
    );
    let gl_uniform1f = f!(sh.gles, "glUniform1f", extern "C" fn(i32, f32));
    let gl_uniform1i = f!(sh.gles, "glUniform1i", extern "C" fn(i32, i32));
    let gl_get_uniformfv = f!(sh.gles, "glGetUniformfv", extern "C" fn(u32, i32, *mut f32));
    let gl_get_uniformiv = f!(sh.gles, "glGetUniformiv", extern "C" fn(u32, i32, *mut i32));
    let gl_get_uniformuiv = f!(
        sh.gles,
        "glGetUniformuiv",
        extern "C" fn(u32, i32, *mut u32)
    );

    // A VS with two attributes (aPos vec2, aColor vec3) and two data uniforms (uTint vec4, uScale float),
    // and an FS with one sampler (uTex). All reflection is enumerated declaration-order, data-first.
    const VS: &str = "attribute vec2 aPos;\nattribute vec3 aColor;\nuniform vec4 uTint;\nuniform float uScale;\nvoid main(){ gl_Position = vec4(aPos*uScale, aColor.x, 1.0) + uTint; }\n";
    const FS: &str = "precision mediump float;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vec2(0.5)); }\n";

    let compile = |kind: u32, src: &str| -> u32 {
        let s = gl_create_shader(kind);
        let c = std::ffi::CString::new(src).unwrap();
        let ptr = c.as_ptr();
        // count=1, length=null => the shim strlen()s the single NUL-terminated string.
        gl_shader_source(s, 1, &ptr, core::ptr::null());
        gl_compile_shader(s);
        // glGetShaderiv marshalling: COMPILE_STATUS true, SHADER_TYPE echoes the kind, SOURCE_LENGTH = len+1.
        let mut cs: i32 = -1;
        gl_get_shaderiv(s, GL_COMPILE_STATUS, &mut cs);
        assert_eq!(cs, GL_TRUE, "shader compiles");
        let mut ty: i32 = -1;
        gl_get_shaderiv(s, GL_SHADER_TYPE, &mut ty);
        assert_eq!(ty as u32, kind, "GL_SHADER_TYPE echoes the created kind");
        let mut sl: i32 = -1;
        gl_get_shaderiv(s, GL_SHADER_SOURCE_LENGTH, &mut sl);
        assert_eq!(
            sl,
            src.len() as i32 + 1,
            "GL_SHADER_SOURCE_LENGTH counts the NUL"
        );
        s
    };
    let vs = compile(GL_VERTEX_SHADER, VS);
    let fs = compile(GL_FRAGMENT_SHADER, FS);
    let prog = gl_create_program();
    gl_attach_shader(prog, vs);
    gl_attach_shader(prog, fs);
    gl_link_program(prog);
    gl_use_program(prog);

    // glGetProgramiv: link status + reflected counts (2 attrs, 3 uniforms = 2 data + 1 sampler, 2 shaders).
    let getprog = |p: u32| {
        let mut v: i32 = -1;
        gl_get_programiv(prog, p, &mut v);
        v
    };
    assert_eq!(getprog(GL_LINK_STATUS), GL_TRUE);
    assert_eq!(getprog(GL_VALIDATE_STATUS), GL_TRUE);
    assert_eq!(getprog(GL_ATTACHED_SHADERS), 2);
    assert_eq!(getprog(GL_ACTIVE_ATTRIBUTES), 2, "aPos + aColor");
    assert_eq!(getprog(GL_ACTIVE_UNIFORMS), 3, "uTint + uScale + uTex");
    assert_eq!(getprog(GL_INFO_LOG_LENGTH), 0);

    // glGetAttribLocation: declaration-order slots.
    let loc = |func: extern "C" fn(u32, *const c_char) -> i32, name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        func(prog, c.as_ptr())
    };
    assert_eq!(loc(gl_get_attrib_location, "aPos"), 0);
    assert_eq!(loc(gl_get_attrib_location, "aColor"), 1);
    assert_eq!(
        loc(gl_get_attrib_location, "nope"),
        -1,
        "unknown attribute -> -1"
    );

    // glGetUniformLocation: data uniforms indexed first (uTint=0, uScale=1); the sampler uses a SEPARATE
    // index space (uTex=0 among samplers — the shim's modeled location convention).
    assert_eq!(loc(gl_get_uniform_location, "uTint"), 0);
    assert_eq!(loc(gl_get_uniform_location, "uScale"), 1);
    assert_eq!(
        loc(gl_get_uniform_location, "uTex"),
        0,
        "sampler location space is separate"
    );
    assert_eq!(loc(gl_get_uniform_location, "nope"), -1);

    // glGetActiveAttrib: name + GL type + size for each attribute (real reflection into 4 out-params).
    let active = |func: extern "C" fn(u32, u32, i32, *mut i32, *mut i32, *mut u32, *mut c_char),
                  index: u32|
     -> (String, u32, i32, i32) {
        let mut namebuf = [0 as c_char; 64];
        let mut len: i32 = -1;
        let mut size: i32 = -1;
        let mut ty: u32 = 0;
        func(
            prog,
            index,
            namebuf.len() as i32,
            &mut len,
            &mut size,
            &mut ty,
            namebuf.as_mut_ptr(),
        );
        let name = unsafe { std::ffi::CStr::from_ptr(namebuf.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        (name, ty, size, len)
    };
    let a0 = active(gl_get_active_attrib, 0);
    assert_eq!((a0.0.as_str(), a0.1, a0.2), ("aPos", GL_FLOAT_VEC2, 1));
    assert_eq!(
        a0.3, 4,
        "GL_ACTIVE_ATTRIB length excludes the NUL (\"aPos\" = 4)"
    );
    let a1 = active(gl_get_active_attrib, 1);
    assert_eq!((a1.0.as_str(), a1.1, a1.2), ("aColor", GL_FLOAT_VEC3, 1));

    // glGetActiveUniform: data uniforms first (uTint vec4, uScale float), then the sampler (uTex).
    let u0 = active(gl_get_active_uniform, 0);
    assert_eq!((u0.0.as_str(), u0.1, u0.2), ("uTint", GL_FLOAT_VEC4, 1));
    let u1 = active(gl_get_active_uniform, 1);
    assert_eq!((u1.0.as_str(), u1.1, u1.2), ("uScale", GL_FLOAT, 1));
    let u2 = active(gl_get_active_uniform, 2);
    assert_eq!((u2.0.as_str(), u2.1, u2.2), ("uTex", GL_SAMPLER_2D, 1));

    // glGetActiveUniform on an out-of-range index raises GL_INVALID_VALUE + empty name (never OOB).
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);
    let _ = gl_get_error();
    let oob = active(gl_get_active_uniform, 99);
    assert_eq!(oob.0, "", "out-of-range active uniform has an empty name");
    assert_eq!(gl_get_error(), GL_INVALID_VALUE);

    // glGetActiveAttrib name truncation: a tiny buffer writes n-1 chars + NUL, length = n-1.
    let mut tiny = [0 as c_char; 3];
    let mut tlen: i32 = -1;
    gl_get_active_attrib(
        prog,
        1,
        tiny.len() as i32,
        &mut tlen,
        &mut 0,
        &mut 0,
        tiny.as_mut_ptr(),
    );
    let tname = unsafe { std::ffi::CStr::from_ptr(tiny.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        tname, "aC",
        "name truncated to bufSize-1 with a NUL terminator"
    );
    assert_eq!(
        tlen, 2,
        "reported length excludes the NUL and matches the truncation"
    );

    // glGetUniformfv / iv / uiv readback of a set data uniform (uScale = 2.5 at location 1).
    gl_uniform1f(1, 2.5);
    let mut fv: f32 = -1.0;
    gl_get_uniformfv(prog, 1, &mut fv);
    assert_eq!(fv, 2.5, "glGetUniformfv reads back the set data uniform");
    // uTint (vec4) at location 0 — read all 4 components back.
    gl_uniform4f(0, 0.1, 0.2, 0.3, 0.4);
    let mut tint = [-1f32; 4];
    gl_get_uniformfv(prog, 0, tint.as_mut_ptr());
    assert_eq!(tint, [0.1, 0.2, 0.3, 0.4]);
    // The integer bit-pattern reinterpretation (glGetUniformiv/uiv share the same byte copy).
    let mut iv: i32 = 0;
    gl_get_uniformiv(prog, 1, &mut iv);
    assert_eq!(
        f32::from_bits(iv as u32),
        2.5,
        "glGetUniformiv copies the same bytes"
    );
    let mut uiv: u32 = 0;
    gl_get_uniformuiv(prog, 1, &mut uiv);
    assert_eq!(f32::from_bits(uiv), 2.5);

    // glGetAttachedShaders: the real vs+fs attachment names.
    let mut names = [0u32; 4];
    let mut cnt: i32 = -1;
    gl_get_attached_shaders(prog, names.len() as i32, &mut cnt, names.as_mut_ptr());
    assert_eq!(cnt, 2, "two shaders attached");
    let got: Vec<u32> = names[..2].to_vec();
    assert!(
        got.contains(&vs) && got.contains(&fs),
        "attached names are vs+fs, got {got:?}"
    );

    // glGet{Program,Shader}InfoLog: a clean link/compile => empty NUL-terminated log, length 0.
    let mut log = [0x7F as c_char; 32];
    let mut loglen: i32 = -1;
    gl_get_program_info_log(prog, log.len() as i32, &mut loglen, log.as_mut_ptr());
    assert_eq!(loglen, 0, "clean link log is empty");
    assert_eq!(log[0], 0, "info log is NUL-terminated");
    gl_get_shader_info_log(vs, log.len() as i32, &mut loglen, log.as_mut_ptr());
    assert_eq!(loglen, 0);

    // A sampler-only program exercises the glGetUniformiv sampler-unit readback path unambiguously
    // (no data uniform shadows location 0).
    let vs2 = compile(
        GL_VERTEX_SHADER,
        "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos,0.0,1.0); }\n",
    );
    let fs2 = compile(GL_FRAGMENT_SHADER, FS);
    let prog2 = gl_create_program();
    gl_attach_shader(prog2, vs2);
    gl_attach_shader(prog2, fs2);
    gl_link_program(prog2);
    gl_use_program(prog2);
    // glUniform1i binds the sampler (declaration index 0) to texture unit 3; glGetUniformiv reads it back.
    gl_uniform1i(0, 3);
    let mut unit: i32 = -1;
    gl_get_uniformiv(prog2, 0, &mut unit);
    assert_eq!(
        unit, 3,
        "glGetUniformiv on a sampler reports its bound texture unit"
    );
}

// ==================================================================================================
// 5) Object state queries: glGetTexParameter* / glGetBufferParameter* / glGetRenderbufferParameteriv /
//    glGetFramebufferAttachmentParameteriv / glGetTexLevelParameteriv — assert real recorded state.
// ==================================================================================================
