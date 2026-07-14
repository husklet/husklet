//! GLSL-ES front-end — reflection + a GLSL-ES → naga-acceptable *desktop* GLSL rewrite.
//!
//! The host owns the shader compiler (naga's `glsl-in` on the wgpu executor), so the guest driver FORWARDS
//! GLSL source rather than pre-translating to a backend IR. naga's `glsl-in` accepts only DESKTOP GLSL
//! (`#version 440+`, `layout`-qualified `in`/`out`, explicit fragment outputs, `layout(binding=)` uniform
//! blocks) — not the GLES `attribute`/`varying`/`gl_FragColor`/`#version N es` dialect — so
//! [`translate_render`] regenerates each stage's DECLARATIONS into the desktop form from the reflected
//! interface and carries the shader BODY through (desktop GLSL is a superset of the ES body syntax). Each
//! stage is packed into its own `GlslDescriptor` (`ShaderPayloadKind::Glsl`) at `glLinkProgram`. The public
//! reflection helpers ([`collect_vertex_attrs`], [`uni_layout`], [`program_samplers`]) feed the pipeline's
//! vertex layout + the uniform/sampler bind-group emission at swap.

/// A parsed `qualifier TYPE name;` declaration (gl_shim.c `struct decl`).
#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    pub ty: String,
    pub name: String,
}

/// A uniform-block member's byte offset/size (gl_shim.c `struct uni`).
#[derive(Clone, Debug, PartialEq)]
pub struct Uni {
    pub name: String,
    pub off: i32,
    pub sz: i32,
}

// ---------------------------------------------------------------------------------------------------
// GLSL-ES compute → naga-acceptable desktop GLSL compute (the CreateShader Glsl payload)
// ---------------------------------------------------------------------------------------------------

/// GLSL-ES compute (`GL_COMPUTE_SHADER`) → desktop GLSL the host compiles. We FORWARD the source (the host
/// owns the compiler — naga on the wgpu executor) rather than pre-translating to a backend IR: strip
/// comments + any ES `#version … es` directive and pin a desktop `#version`, so naga's `glsl-in` accepts
/// it. The entry point stays `main` in-source and is renamed to the pipeline-bound `cmain` host-side. The
/// software oracle does not execute a GLSL compute payload (it runs only neutral KERNEL programs), so this
/// is asserted at the `Cmd` level; on wgpu it is a real compute module.
pub fn translate_compute(cs_in: &str) -> String {
    let mut body = strip_version(&strip_comments(cs_in));
    strip_es_precision(&mut body);
    let mut out = String::new();
    out.push_str(GLSL_VERSION);
    out.push_str(&body);
    out
}

// ---------------------------------------------------------------------------------------------------
// GLSL-ES → naga-acceptable desktop GLSL (the CreateShader Glsl payload)
//
// naga's `glsl-in` (the host compiler on the wgpu executor) accepts only DESKTOP GLSL (>= 440) with
// `layout`-qualified `in`/`out`, explicit fragment outputs and `layout(binding=)` uniform blocks — NOT the
// GLES `attribute`/`varying`/`gl_FragColor`/`#version N es` dialect. So the driver forwards GLSL (source,
// not a backend IR) but regenerates each stage's DECLARATIONS from the reflected interface into the desktop
// form, carrying the shader BODY through (desktop GLSL is a superset of the ES body syntax, modulo ES
// precision qualifiers and the `texture2D` builtin). The host compiles the result.
// ---------------------------------------------------------------------------------------------------

/// The desktop GLSL version naga's `glsl-in` accepts (440/450/460; ES profiles are rejected).
const GLSL_VERSION: &str = "#version 460\n";

/// Strip a leading `#version …` directive line (ES or desktop) so we can pin our own desktop version.
fn strip_version(src: &str) -> String {
    let mut out = String::new();
    for line in src.lines() {
        if line.trim_start().starts_with("#version") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Remove ES precision qualifiers from a shader body — invalid as qualifiers in desktop core GLSL.
fn strip_es_precision(body: &mut String) {
    wreplace(body, "lowp", "");
    wreplace(body, "mediump", "");
    wreplace(body, "highp", "");
}

/// Emit the data-uniform interface block at `binding = 0` (matching the frame's uniform bind entry). An
/// anonymous block puts its members in global scope so the shader body references them by their plain name.
/// The sampler texture/sampler bindings start at 1 ([`emit_sampler_decls`]) so the UBO never collides.
fn emit_uniform_block(out: &mut String, unis: &[Decl]) {
    if unis.is_empty() {
        return;
    }
    out.push_str("layout(std140, binding = 0) uniform HlUniforms {\n");
    for u in unis {
        out.push_str(&format!("    {} {};\n", u.ty, u.name));
    }
    out.push_str("};\n");
}

/// Split a GLSL-ES combined-sampler type into naga's separate `(texture type, sampler type, recombining
/// constructor)`. naga's `glsl-in` REJECTS a combined `uniform sampler2D` at global scope (it errors with
/// `NotImplemented("variable qualifier")`), accepting only the Vulkan-flavored form: a `texture2D` global +
/// a `sampler` global recombined at each use site by a `sampler2D(tex, samp)` constructor. So every sampler
/// is emitted as that pair and its uses rewritten by [`rewrite_sampler_refs`].
fn split_sampler(ty: &str) -> (&'static str, &'static str, &'static str) {
    match ty {
        "samplerCube" => ("textureCube", "sampler", "samplerCube"),
        "sampler2DShadow" => ("texture2D", "samplerShadow", "sampler2DShadow"),
        _ => ("texture2D", "sampler", "sampler2D"),
    }
}

/// Emit each combined image-sampler as a SEPARATE `texture2D` + `sampler` pair (naga rejects a combined
/// `uniform sampler2D`). The uniform block owns binding 0; sampler `k` (declaration index) owns TEXTURE
/// binding `1 + 2k` and SAMPLER binding `2 + 2k` — every UBO/texture/sampler thus lands on a DISTINCT
/// binding within the single wgpu bind-group namespace, exactly matching the `BindEntry`s
/// [`crate::service::frame::build_frame_ir`] emits. The shader body recombines the pair at each use via
/// [`rewrite_sampler_refs`].
fn emit_sampler_decls(out: &mut String, samps: &[Decl]) {
    for (k, s) in samps.iter().enumerate() {
        let (tex_ty, smp_ty, _) = split_sampler(&s.ty);
        let tex_binding = 1 + 2 * k;
        let smp_binding = 2 + 2 * k;
        out.push_str(&format!("layout(binding = {tex_binding}) uniform {tex_ty} {}_hltex;\n", s.name));
        out.push_str(&format!("layout(binding = {smp_binding}) uniform {smp_ty} {}_hlsmp;\n", s.name));
    }
}

/// Rewrite each combined-sampler NAME in a shader body to a `ctor(name_hltex, name_hlsmp)` expression, so a
/// `texture(uTex, uv)` call feeds the separated texture + sampler globals [`emit_sampler_decls`] declared.
/// A sampler uniform can only ever appear as a texture-function argument (you cannot do arithmetic on a
/// sampler), so a word-boundary name replace is safe and total. Run BEFORE the `texture2D(`→`texture(`
/// lowering: `texture2D(uTex, uv)` → `texture2D(sampler2D(uTex_hltex, uTex_hlsmp), uv)` → `texture(...)`.
fn rewrite_sampler_refs(body: &mut String, samps: &[Decl]) {
    for s in samps {
        let (_, _, ctor) = split_sampler(&s.ty);
        let repl = format!("{ctor}({}_hltex, {}_hlsmp)", s.name, s.name);
        wreplace(body, &s.name, &repl);
    }
}

/// Translate a vertex+fragment GLSL-ES pair into the naga-acceptable desktop GLSL for each stage,
/// returned as `(vertex_glsl, fragment_glsl)`. Each is packed into its own `Glsl` `CreateShader` payload
/// (see [`crate::model::program::Program::link`]); the render pipeline binds them as separate modules.
pub fn translate_render(vs_in: &str, fs_in: &str) -> (String, String) {
    let vs = strip_comments(vs_in);
    let fs = strip_comments(fs_in);

    let attrs = collect_vertex_attrs(&vs);
    let mut vary = collect(&vs, "varying");
    vary.truncate(16);
    append_decls_unique(&mut vary, collect(&vs, "out"), 16);
    let (unis, samps) = collect_uniforms(&vs, &fs);
    let mut fragouts = collect(&fs, "out");
    fragouts.truncate(4);

    let mut consts = collect_consts(&vs);
    for c in collect_consts(&fs) {
        if consts.len() >= 16 {
            break;
        }
        if !consts.iter().any(|x| *x == c) {
            consts.push(c);
        }
    }

    // ---- vertex stage ----
    let mut vs_out = String::new();
    vs_out.push_str(GLSL_VERSION);
    for c in &consts {
        vs_out.push_str(c);
        vs_out.push('\n');
    }
    for (i, a) in attrs.iter().enumerate() {
        vs_out.push_str(&format!("layout(location = {i}) in {} {};\n", a.ty, a.name));
    }
    for (j, v) in vary.iter().enumerate() {
        vs_out.push_str(&format!("layout(location = {j}) out {} {};\n", v.ty, v.name));
    }
    emit_uniform_block(&mut vs_out, &unis);
    emit_sampler_decls(&mut vs_out, &samps);
    let mut vb = main_body(&vs);
    strip_es_precision(&mut vb);
    rewrite_sampler_refs(&mut vb, &samps);
    sreplace(&mut vb, "texture2D(", "texture(");
    sreplace(&mut vb, "textureCube(", "texture(");
    vs_out.push_str(&format!("void main() {{\n{vb}\n}}\n"));

    // ---- fragment stage ----
    let mut fs_out = String::new();
    fs_out.push_str(GLSL_VERSION);
    for c in &consts {
        fs_out.push_str(c);
        fs_out.push('\n');
    }
    for (j, v) in vary.iter().enumerate() {
        fs_out.push_str(&format!("layout(location = {j}) in {} {};\n", v.ty, v.name));
    }
    emit_uniform_block(&mut fs_out, &unis);
    emit_sampler_decls(&mut fs_out, &samps);
    // The fragment output: reuse the ES3 `out vec4 NAME;` if declared, else synthesize one and rewrite the
    // ES2 `gl_FragColor` builtin onto it (desktop core GLSL has no `gl_FragColor`).
    let frag_name = fragouts
        .first()
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "hl_FragColor".to_string());
    fs_out.push_str(&format!("layout(location = 0) out vec4 {frag_name};\n"));
    let mut fb = main_body(&fs);
    strip_es_precision(&mut fb);
    rewrite_sampler_refs(&mut fb, &samps);
    sreplace(&mut fb, "texture2D(", "texture(");
    sreplace(&mut fb, "textureCube(", "texture(");
    if fragouts.is_empty() {
        wreplace(&mut fb, "gl_FragColor", &frag_name);
    }
    fs_out.push_str(&format!("void main() {{\n{fb}\n}}\n"));

    (vs_out, fs_out)
}

// ---------------------------------------------------------------------------------------------------
// GLSL parsing / reflection helpers (shared by the desktop-GLSL emit above and the query/introspection
// reflection). Ported from hl-shim-gl/src/translate.rs.
// ---------------------------------------------------------------------------------------------------

#[inline]
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}
#[inline]
fn is_word(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}
fn is_precision_or_interp(t: &str) -> bool {
    matches!(t, "lowp" | "mediump" | "highp" | "flat" | "smooth" | "centroid")
}

/// `collect` — parse `kw TYPE name;` declarations from `src` (skipping precision/interpolation
/// qualifiers before the type).
fn collect(src: &str, kw: &str) -> Vec<Decl> {
    let b = src.as_bytes();
    let kb = kw.as_bytes();
    let kl = kb.len();
    let mut out = Vec::new();
    let mut p = 0usize;
    while let Some(rel) = find_from(b, kb, p) {
        let at = rel;
        let before_word = at != 0 && is_word(b[at - 1]);
        let after_word = at + kl < b.len() && is_word(b[at + kl]);
        if before_word || after_word {
            p = at + kl;
            continue;
        }
        let mut q = at + kl;
        while q < b.len() && is_space(b[q]) {
            q += 1;
        }
        let read_tok = |q: &mut usize| -> String {
            let mut s = String::new();
            while *q < b.len() && !is_space(b[*q]) && b[*q] != b';' && s.len() < 15 {
                s.push(b[*q] as char);
                *q += 1;
            }
            s
        };
        let mut ty = read_tok(&mut q);
        while is_precision_or_interp(&ty) {
            while q < b.len() && is_space(b[q]) {
                q += 1;
            }
            ty = read_tok(&mut q);
        }
        while q < b.len() && is_space(b[q]) {
            q += 1;
        }
        // std140 interface block: `uniform Block { TYPE m; ... } [inst];` — enumerate the MEMBERS.
        if q < b.len() && b[q] == b'{' {
            q += 1;
            while out.len() < 32 {
                while q < b.len() && is_space(b[q]) {
                    q += 1;
                }
                if q >= b.len() || b[q] == b'}' {
                    break;
                }
                let mut mty = read_tok(&mut q);
                while is_precision_or_interp(&mty) {
                    while q < b.len() && is_space(b[q]) {
                        q += 1;
                    }
                    mty = read_tok(&mut q);
                }
                while q < b.len() && is_space(b[q]) {
                    q += 1;
                }
                let mut mnm = String::new();
                while q < b.len() && is_word(b[q]) && mnm.len() < 31 {
                    mnm.push(b[q] as char);
                    q += 1;
                }
                while q < b.len() && b[q] != b';' && b[q] != b'}' {
                    q += 1; // skip any array subscript to the member end
                }
                if q < b.len() && b[q] == b';' {
                    q += 1;
                }
                if !mty.is_empty() && !mnm.is_empty() {
                    out.push(Decl { ty: mty, name: mnm });
                }
            }
            if q < b.len() && b[q] == b'}' {
                q += 1;
            }
            while q < b.len() && b[q] != b';' {
                q += 1; // skip the optional instance name
            }
            if q < b.len() && b[q] == b';' {
                q += 1;
            }
            p = q;
            continue;
        }
        let mut nm = String::new();
        while q < b.len() && is_word(b[q]) && nm.len() < 31 {
            nm.push(b[q] as char);
            q += 1;
        }
        if !ty.is_empty() && !nm.is_empty() {
            out.push(Decl { ty, name: nm });
        }
        p = q;
    }
    out
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// Extract the body between `void main(){` and the matching final `}` (gl_shim.c `main_body`).
fn main_body(src: &str) -> String {
    let b = src.as_bytes();
    let p = find_from(b, b"main", 0).and_then(|m| b[m..].iter().position(|&c| c == b'{').map(|i| m + i));
    let e = b.iter().rposition(|&c| c == b'}');
    match (p, e) {
        (Some(p), Some(e)) if e > p => String::from_utf8_lossy(&b[p + 1..e]).into_owned(),
        _ => String::new(),
    }
}

/// Word-boundary replace `from`→`to` (gl_shim.c `wreplace`).
fn wreplace(buf: &mut String, from: &str, to: &str) {
    let b = buf.as_bytes();
    let fb = from.as_bytes();
    let fl = fb.len();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if i + fl <= n && &b[i..i + fl] == fb {
            let before = if i > 0 { b[i - 1] } else { b' ' };
            let after = if i + fl < n { b[i + fl] } else { 0 };
            if !is_word(before) && !is_word(after) {
                out.push_str(to);
                i += fl;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    *buf = out;
}

/// Plain substring replace `from`→`to` (gl_shim.c `sreplace`).
fn sreplace(buf: &mut String, from: &str, to: &str) {
    let b = buf.as_bytes();
    let fb = from.as_bytes();
    let fl = fb.len();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if i + fl <= n && &b[i..i + fl] == fb {
            out.push_str(to);
            i += fl;
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    *buf = out;
}

/// Global `const TYPE name = …;` declarations before `main()` (gl_shim.c `collect_consts`).
fn collect_consts(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let end = find_from(b, b"main", 0);
    let mut out = Vec::new();
    let mut p = 0usize;
    while let Some(at) = find_from(b, b"const", p) {
        if let Some(e) = end {
            if at >= e {
                break;
            }
        }
        let before = if at == 0 { b' ' } else { b[at - 1] };
        let after = b.get(at + 5).copied().unwrap_or(0);
        if is_word(before) || (after != b' ' && after != b'\t') {
            p = at + 5;
            continue;
        }
        match b[at..].iter().position(|&c| c == b';') {
            Some(semi_rel) => {
                let semi = at + semi_rel;
                out.push(String::from_utf8_lossy(&b[at..=semi]).into_owned());
                p = semi + 1;
            }
            None => break,
        }
    }
    out
}

/// Strip `//` and `/* */` comments (gl_shim.c `strip_comments`).
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut r = 0;
    while r < n {
        if r + 1 < n && b[r] == b'/' && b[r + 1] == b'/' {
            while r < n && b[r] != b'\n' {
                r += 1;
            }
        } else if r + 1 < n && b[r] == b'/' && b[r + 1] == b'*' {
            r += 2;
            while r + 1 < n && !(b[r] == b'*' && b[r + 1] == b'/') {
                r += 1;
            }
            if r + 1 < n {
                r += 2;
            } else {
                r = n;
            }
        } else {
            out.push(b[r] as char);
            r += 1;
        }
    }
    out
}

fn is_sampler_type(t: &str) -> bool {
    matches!(t, "sampler2D" | "samplerCube" | "sampler2DShadow")
}

/// Collect uniforms from vs+fs (dedup by name), split into DATA uniforms and SAMPLER uniforms
/// (gl_shim.c `collect_uniforms`). Returns `(data, samplers)` capped at 16 / 4.
fn collect_uniforms(vs: &str, fs: &str) -> (Vec<Decl>, Vec<Decl>) {
    let mut all = collect(vs, "uniform");
    all.truncate(32);
    for d in collect(fs, "uniform") {
        if all.len() >= 32 {
            break;
        }
        if !all.iter().any(|x| x.name == d.name) {
            all.push(d);
        }
    }
    let (mut data, mut samps) = (Vec::new(), Vec::new());
    for d in all {
        if is_sampler_type(&d.ty) {
            if samps.len() < 4 {
                samps.push(d);
            }
        } else if data.len() < 16 {
            data.push(d);
        }
    }
    (data, samps)
}

fn append_decls_unique(dst: &mut Vec<Decl>, src: Vec<Decl>, max: usize) {
    for d in src {
        if dst.len() >= max {
            break;
        }
        if !dst.iter().any(|x| x.name == d.name) {
            dst.push(d);
        }
    }
}

/// Vertex attributes: `attribute` decls + `in` decls (unique by name) — gl_shim.c
/// `collect_vertex_attrs`.
pub fn collect_vertex_attrs(vs: &str) -> Vec<Decl> {
    // Strip comments FIRST, matching every sibling reflection helper (`collect_uniforms`,
    // `program_frag_outputs`, …). Without this, a comment mentioning `attribute`/`in` (e.g. a legacy
    // declaration commented out) would be collected as a phantom attribute — shifting the location
    // namespace that `attrib_location`/`active_attrib`/the frame vertex layout key on AWAY from the
    // layout the translator emits (which runs on stripped source). Idempotent when the caller pre-strips.
    let vs = strip_comments(vs);
    let mut attrs = collect(&vs, "attribute");
    attrs.truncate(16);
    let tmp = collect(&vs, "in");
    append_decls_unique(&mut attrs, tmp, 16);
    attrs
}

/// MSL struct member (size, align) for a GLSL uniform type (gl_shim.c `msl_type_layout`).
fn msl_type_layout(t: &str) -> Option<(i32, i32)> {
    Some(match t {
        "float" | "int" | "uint" | "bool" => (4, 4),
        "vec2" | "ivec2" | "uvec2" | "bvec2" => (8, 8),
        "vec3" | "ivec3" | "uvec3" | "bvec3" => (16, 16),
        "vec4" | "ivec4" | "uvec4" | "bvec4" => (16, 16),
        "mat2" | "mat2x2" => (16, 8),
        "mat3" | "mat3x3" => (48, 16),
        "mat4" | "mat4x4" => (64, 16),
        "mat2x3" => (32, 16),
        "mat2x4" => (32, 16),
        "mat3x2" => (24, 8),
        "mat3x4" => (48, 16),
        "mat4x2" => (32, 8),
        "mat4x3" => (64, 16),
        _ => return None,
    })
}

/// Compute the uniform-block byte layout (name→offset/size) matching Metal's struct alignment
/// (gl_shim.c `uni_layout`). Returns `(members, total_bytes)`.
pub fn uni_layout(vs: &str, fs: &str) -> (Vec<Uni>, i32) {
    let (unis, _samps) = collect_uniforms(&strip_comments(vs), &strip_comments(fs));
    let mut cur = 0i32;
    let mut out = Vec::new();
    for d in unis.iter().take(16) {
        let (sz, al) = msl_type_layout(&d.ty).unwrap_or((4, 4));
        cur = (cur + al - 1) & !(al - 1);
        out.push(Uni { name: d.name.clone(), off: cur, sz });
        cur += sz;
    }
    let total = (cur + 15) & !15;
    (out, total)
}

/// The samplers a linked program declares, in declaration order (for `glUniform1i` → texture-unit
/// mapping and the bind-group emission).
pub fn program_samplers(vs: &str, fs: &str) -> Vec<String> {
    let (_data, samps) = collect_uniforms(&strip_comments(vs), &strip_comments(fs));
    samps.into_iter().map(|d| d.name).collect()
}

/// The DATA uniforms a linked program declares, in declaration order, as `(name, glsl_type)` — the
/// reflection `glGetActiveUniform` reports. Matches the order of the uniform-block layout ([`uni_layout`])
/// and the location convention of [`Program::uniform_location`](crate::model::program::Program::uniform_location)
/// (data uniforms first, then samplers), so the two never disagree.
pub fn program_uniform_decls(vs: &str, fs: &str) -> Vec<Decl> {
    let (data, _samps) = collect_uniforms(&strip_comments(vs), &strip_comments(fs));
    data
}

/// The SAMPLER uniforms as `(name, glsl_type)` (`program_samplers` keeps only names; this keeps the
/// declared sampler type so `glGetActiveUniform` can report `GL_SAMPLER_2D` vs `GL_SAMPLER_CUBE`).
pub fn program_sampler_decls(vs: &str, fs: &str) -> Vec<Decl> {
    let (_data, samps) = collect_uniforms(&strip_comments(vs), &strip_comments(fs));
    samps
}

/// The fragment-shader output variables a linked program declares (`out vecN name;`), in declaration
/// order — the resource list `glGetFragDataLocation`/`glGetProgramResource*(GL_PROGRAM_OUTPUT)` resolve
/// against. An ES2-style `gl_FragColor` shader declares none (its single output is location 0 implicitly).
pub fn program_frag_outputs(fs: &str) -> Vec<Decl> {
    let mut outs = collect(&strip_comments(fs), "out");
    outs.truncate(4);
    outs
}
