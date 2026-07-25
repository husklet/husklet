use std::fmt::Write as _;

pub(super) fn emit_stub(out: &mut String, lib: &str, name: &str, ret: &str, params: &str) {
    let mut sig = String::new();
    let mut argnames = Vec::new();
    if !params.is_empty() {
        for (i, p) in params.split(';').enumerate() {
            let Some((ty, pname)) = p.split_once('|') else {
                panic!("bad param {p:?} in {name}");
            };
            let rty = Binding::new(ty.trim(), name).rust_type();
            let pn = Signature::parameter(pname.trim(), i);
            if i > 0 {
                sig.push_str(", ");
            }
            write!(sig, "{pn}: {rty}").unwrap();
            argnames.push(pn);
        }
    }
    let rmap = Binding::new(ret.trim(), name).return_type();
    let arrow = rmap
        .as_ref()
        .map(|r| format!(" -> {r}"))
        .unwrap_or_default();
    let touch: String = argnames.iter().map(|a| format!("let _ = {a}; ")).collect();
    let body = match &rmap {
        None => format!("{touch}crate::stub::hit(\"{name}\");"),
        Some(r) => format!(
            "{touch}crate::stub::hit(\"{name}\"); {}",
            Binding::default_value(r)
        ),
    };
    // Export the symbol only from the object that OWNS this half of the surface: `gl*` from libGLESv2
    // (`cfg(gles_client)`), `egl*` from libEGL (`cfg(not(gles_client))`). The other object still COMPILES
    // the fn (so `eglGetProcAddress` can return its address) — it is just not `#[no_mangle]` there, so
    // rustc's cdylib export list omits it. This is what pins each `.so`'s exported dynsym set.
    let export_cfg = match lib {
        "GL" => "#[cfg_attr(gles_client, no_mangle)]",
        "EGL" => "#[cfg_attr(not(gles_client), no_mangle)]",
        other => panic!("emit_stub: unknown lib {other:?} for {name}"),
    };
    writeln!(
        out,
        "{export_cfg}\npub extern \"C\" fn {name}({sig}){arrow} {{ {body} }}\n"
    )
    .unwrap();
}

struct Binding<'a> {
    c: &'a str,
    context: &'a str,
}

impl<'a> Binding<'a> {
    fn new(c: &'a str, context: &'a str) -> Self {
        Self { c, context }
    }

    fn return_type(&self) -> Option<String> {
        if self.c == "void" {
            return None;
        }
        Some(self.rust_type())
    }

    fn rust_type(&self) -> String {
        let c = self.c.trim();
        if let Some(base) = c.strip_suffix("*const*") {
            return format!("*const *const {}", Self::pointee(base, self.context));
        }
        if let Some(base) = c.strip_suffix("**") {
            return format!("*mut *mut {}", Self::pointee(base, self.context));
        }
        if let Some(base) = c.strip_suffix('*') {
            let is_const = base.trim_start().starts_with("const ");
            let q = if is_const { "*const" } else { "*mut" };
            return format!("{q} {}", Self::pointee(base, self.context));
        }
        Self::scalar(c, self.context).to_string()
    }

    fn pointee(base: &str, context: &str) -> String {
        let b = base.trim();
        let b = b.strip_prefix("const ").unwrap_or(b).trim();
        Self::scalar(b, context).to_string()
    }

    /// A bare (by-value) scalar / opaque-handle C type -> Rust C-ABI type. Opaque handles (`EGLDisplay`,
    /// `GLsync`, `EGLConfig`, …) are `void*`-shaped, so a value-of-that-type is a `*mut c_void`. Panics on an
    /// unknown base type so an API bump surfaces at build time rather than silently mis-typing the ABI.
    fn scalar(c: &str, context: &str) -> &'static str {
        match c {
            "void" => "core::ffi::c_void", // only meaningful as a pointee; bare `void` -> map_ret == None
            "char" | "GLchar" => "core::ffi::c_char",
            "GLbyte" => "i8",
            "GLubyte" | "GLboolean" => "u8",
            "GLshort" => "i16",
            "GLushort" => "u16",
            "GLenum" | "GLbitfield" | "GLuint" | "EGLenum" | "EGLBoolean" => "u32",
            "GLint" | "GLsizei" | "GLfixed" | "EGLint" => "i32",
            "GLfloat" | "GLclampf" => "f32",
            "GLdouble" => "f64",
            "GLint64" => "i64",
            "GLuint64" | "EGLTime" | "EGLTimeKHR" | "EGLuint64KHR" => "u64",
            "GLintptr" | "GLsizeiptr" | "EGLAttrib" => "isize",
            // opaque handles (all `void*`-shaped)
            "GLsync"
            | "GLDEBUGPROC"
            | "EGLClientBuffer"
            | "EGLContext"
            | "EGLDisplay"
            | "EGLImage"
            | "EGLImageKHR"
            | "EGLSurface"
            | "EGLSync"
            | "EGLSyncKHR"
            | "EGLConfig"
            | "EGLNativeDisplayType"
            | "EGLNativeWindowType"
            | "EGLNativePixmapType"
            | "__eglMustCastToProperFunctionPointerType" => "*mut core::ffi::c_void",
            other => {
                panic!("unmapped C base type {other:?} (in {context}); extend scalar() in build.rs")
            }
        }
    }

    fn default_value(rust_ty: &str) -> &'static str {
        if rust_ty.starts_with("*const") {
            "core::ptr::null()"
        } else if rust_ty.starts_with("*mut") {
            "core::ptr::null_mut()"
        } else {
            "0" // spec default: GL_NO_ERROR / EGL_FALSE / 0 handle — a stub performs no work.
        }
    }
}

struct Signature;
impl Signature {
    fn parameter(name: &str, i: usize) -> String {
        const KW: &[&str] = &[
            "type", "ref", "box", "in", "fn", "let", "match", "move", "mut", "as", "impl", "loop",
            "where", "self", "final", "override", "become",
        ];
        if name.is_empty() {
            return format!("a{i}");
        }
        if KW.contains(&name) {
            return format!("{name}_");
        }
        name.to_string()
    }
}
