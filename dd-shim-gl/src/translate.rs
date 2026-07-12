//! GLSL-ES → MSL translator — a byte-for-byte Rust port of `gl_shim.c`'s `translate()` + helpers.
//!
//! The host compiles MSL (not GLSL), so the guest shim transforms a vertex+fragment GLSL-ES pair into
//! one combined MSL source at `glLinkProgram`. Every transform here mirrors the C shim exactly (same
//! passes, same order, same whitespace) so the emitted MSL — and therefore the `CreateShader` IR — is
//! identical. Verified against gl_shim.c's own `-DDD_TR_TOOL gl_tr` tool in `tests/translate_parity`.

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

fn gl_type_to_msl(t: &str) -> String {
    match t {
        "vec2" => "float2",
        "vec3" => "float3",
        "vec4" => "float4",
        "ivec2" => "int2",
        "ivec3" => "int3",
        "ivec4" => "int4",
        "uvec2" => "uint2",
        "uvec3" => "uint3",
        "uvec4" => "uint4",
        "mat2" | "mat2x2" => "float2x2",
        "mat3" | "mat3x3" => "float3x3",
        "mat4" | "mat4x4" => "float4x4",
        "mat2x3" => "float2x3",
        "mat3x2" => "float3x2",
        "mat2x4" => "float2x4",
        "mat4x2" => "float4x2",
        "mat3x4" => "float3x4",
        "mat4x3" => "float4x3",
        _ => return t.to_string(),
    }
    .to_string()
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
        // word-boundary: char before must not be a word char, char after keyword must not be either
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

/// Word-boundary replace `from`→`to` (gl_shim.c `wreplace`). Before/after context is read from the
/// ORIGINAL string (a prior replacement in the same pass does not change adjacency).
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

fn type_fixups(b: &mut String) {
    wreplace(b, "lowp", "");
    wreplace(b, "mediump", "");
    wreplace(b, "highp", "");
    wreplace(b, "vec2", "float2");
    wreplace(b, "vec3", "float3");
    wreplace(b, "vec4", "float4");
    wreplace(b, "ivec2", "int2");
    wreplace(b, "ivec3", "int3");
    wreplace(b, "ivec4", "int4");
    wreplace(b, "uvec2", "uint2");
    wreplace(b, "uvec3", "uint3");
    wreplace(b, "uvec4", "uint4");
    sreplace(b, "mat3x2(", "dd_mat3x2(");
    wreplace(b, "mat2x2", "float2x2");
    wreplace(b, "mat2x3", "float2x3");
    wreplace(b, "mat2x4", "float2x4");
    wreplace(b, "mat3x2", "float3x2");
    wreplace(b, "mat3x3", "float3x3");
    wreplace(b, "mat3x4", "float3x4");
    wreplace(b, "mat4x2", "float4x2");
    wreplace(b, "mat4x3", "float4x3");
    wreplace(b, "mat4x4", "float4x4");
    wreplace(b, "mat2", "float2x2");
    wreplace(b, "mat3", "float3x3");
    wreplace(b, "mat4", "float4x4");
}

/// `fn(a, b)` → `((a) op (b))` for the top-level 2-arg case (gl_shim.c `call2_fixup`).
fn call2_fixup(buf: &mut String, func: &str, op: &str) {
    let b = buf.as_bytes();
    let fb = func.as_bytes();
    let fl = fb.len();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if i + fl < n && &b[i..i + fl] == fb && b[i + fl] == b'(' {
            let before = if i > 0 { b[i - 1] } else { b' ' };
            if !is_word(before) {
                let a0 = i + fl + 1;
                let mut j = a0;
                let mut comma = 0usize;
                let mut depth = 1i32;
                while j < n && depth != 0 {
                    match b[j] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        b',' if depth == 1 && comma == 0 => comma = j,
                        _ => {}
                    }
                    j += 1;
                }
                if j < n && b[j] == b')' && comma != 0 {
                    out.push('(');
                    out.push('(');
                    out.push_str(&String::from_utf8_lossy(&b[a0..comma]));
                    out.push(')');
                    out.push(' ');
                    out.push_str(op);
                    out.push(' ');
                    out.push('(');
                    out.push_str(&String::from_utf8_lossy(&b[comma + 1..j]));
                    out.push(')');
                    out.push(')');
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    *buf = out;
}

fn relational_fixups(b: &mut String) {
    call2_fixup(b, "greaterThanEqual", ">=");
    call2_fixup(b, "lessThanEqual", "<=");
    call2_fixup(b, "greaterThan", ">");
    call2_fixup(b, "lessThan", "<");
    call2_fixup(b, "notEqual", "!=");
    call2_fixup(b, "equal", "==");
}

/// Rename `fn(...)`→`to(...)` only when it has a top-level comma (2+ args) — gl_shim.c `rename_call2`.
fn rename_call2(buf: &mut String, func: &str, to: &str) {
    let b = buf.as_bytes();
    let fb = func.as_bytes();
    let fl = fb.len();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if i + fl < n && &b[i..i + fl] == fb && b[i + fl] == b'(' && (i == 0 || !is_word(b[i - 1])) {
            let mut j = i + fl + 1;
            let mut depth = 1i32;
            let mut comma = false;
            while j < n && depth != 0 {
                match b[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b',' if depth == 1 => comma = true,
                    _ => {}
                }
                j += 1;
            }
            if comma {
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

fn builtin_fixups(b: &mut String) {
    wreplace(b, "dFdx", "dfdx");
    wreplace(b, "dFdy", "dfdy");
    wreplace(b, "inversesqrt", "rsqrt");
    rename_call2(b, "atan", "atan2");
    wreplace(b, "mod", "dd_mod");
}

const DD_MOD_HELPERS: &str = "template<typename T> inline T dd_mod(T x, T y) { return x - y * floor(x / y); }\ninline float2 dd_mod(float2 x, float y) { return x - y * floor(x / y); }\ninline float3 dd_mod(float3 x, float y) { return x - y * floor(x / y); }\ninline float4 dd_mod(float4 x, float y) { return x - y * floor(x / y); }\n";
const DD_MAT3X2_HELPER: &str = "inline float3x2 dd_mat3x2(float3x3 m) { return float3x2(m[0].xy, m[1].xy, m[2].xy); }\ninline float3x2 dd_mat3x2(float2 a, float2 b, float2 c) { return float3x2(a, b, c); }\n";

fn local_decl_fixups(b: &mut String) {
    for ty in ["float", "float2", "float3", "float4", "int", "int2", "int3", "int4", "uint", "uint2", "uint3", "uint4"] {
        sreplace(b, &format!("{ty} in."), &format!("{ty} "));
    }
}

/// Rewrite `vecN( EXPR )` truncations (single top-level arg containing a top-level `*`) to a swizzle
/// `(EXPR).xy`/`.xyz` (gl_shim.c `fix_trunc`). Runs before `type_fixups`.
fn fix_trunc(buf: &mut String) {
    let b = buf.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        let mut nn = 0;
        if b[i..].starts_with(b"vec2(") {
            nn = 2;
        } else if b[i..].starts_with(b"vec3(") {
            nn = 3;
        }
        if nn != 0 {
            let before = if i > 0 { b[i - 1] } else { b' ' };
            if is_word(before) {
                nn = 0;
            }
        }
        if nn != 0 {
            let start = i + 5;
            let mut j = start;
            let mut depth = 1i32;
            let mut topcomma = false;
            let mut topstar = false;
            while j < n && depth != 0 {
                match b[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b',' if depth == 1 => topcomma = true,
                    b'*' if depth == 1 => topstar = true,
                    _ => {}
                }
                j += 1;
            }
            if j < n && b[j] == b')' && !topcomma && topstar {
                out.push('(');
                out.push_str(&String::from_utf8_lossy(&b[start..j]));
                out.push(')');
                out.push_str(if nn == 3 { ".xyz" } else { ".xy" });
                i = j + 1;
                continue;
            }
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

/// Strip `//` and `/* */` comments in place (gl_shim.c `strip_comments`).
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
    let mut attrs = collect(vs, "attribute");
    attrs.truncate(16);
    let tmp = collect(vs, "in");
    append_decls_unique(&mut attrs, tmp, 16);
    attrs
}

/// Append the per-stage texture/sampler MSL params for the samplers a stage's body references
/// (gl_shim.c `emit_samp_params`).
fn emit_samp_params(out: &mut String, body: &str, samps: &[Decl]) {
    for (i, s) in samps.iter().enumerate() {
        if !body.contains(&s.name) {
            continue;
        }
        out.push_str(&format!(
            ", texture2d<float> {} [[texture({})]], sampler {}Smplr [[sampler({})]]",
            s.name, i, s.name, i
        ));
    }
}

/// `texture2D(NAME,` / `texture(NAME,` → `NAME.sample(NAMESmplr,` (gl_shim.c `sampler_fixups`).
fn sampler_fixups(b: &mut String, samps: &[Decl]) {
    for s in samps {
        let to = format!("{}.sample({}Smplr", s.name, s.name);
        sreplace(b, &format!("texture2D({}", s.name), &to);
        sreplace(b, &format!("texture({}", s.name), &to);
    }
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

/// Translate a vertex+fragment GLSL-ES pair into one combined MSL source (gl_shim.c `translate`).
pub fn translate(vs_in: &str, fs_in: &str) -> String {
    let vs = strip_comments(vs_in);
    let fs = strip_comments(fs_in);

    let attrs = collect_vertex_attrs(&vs);
    let mut vary = collect(&vs, "varying");
    vary.truncate(16);
    append_decls_unique(&mut vary, collect(&vs, "out"), 16);
    let mut fragouts = collect(&fs, "out");
    fragouts.truncate(4);
    let (unis, samps) = collect_uniforms(&vs, &fs);
    let mut consts = collect_consts(&vs);
    for c in collect_consts(&fs) {
        if consts.len() >= 16 {
            break;
        }
        if !consts.iter().any(|x| *x == c) {
            consts.push(c);
        }
    }

    let mut out = String::new();
    out.push_str("#include <metal_stdlib>\nusing namespace metal;\n");
    if vs.contains("mod(") || fs.contains("mod(") {
        out.push_str(DD_MOD_HELPERS);
    }
    if vs.contains("mat3x2(") || fs.contains("mat3x2(") {
        out.push_str(DD_MAT3X2_HELPER);
    }
    for c in &consts {
        let mut line = c.clone();
        type_fixups(&mut line);
        if let Some(pos) = line.find("const") {
            let ok = pos == 0 || matches!(line.as_bytes()[pos - 1], b' ' | b'\n');
            if ok {
                out.push_str(&format!("constant {}\n", &line[pos + 5..]));
            }
        }
    }

    let has_u = !unis.is_empty();
    if has_u {
        out.push_str("struct Uniforms {\n");
        for u in &unis {
            out.push_str(&format!("  {} {};\n", gl_type_to_msl(&u.ty), u.name));
        }
        out.push_str("};\n");
    }
    out.push_str("struct VIn {\n");
    for (i, a) in attrs.iter().enumerate() {
        out.push_str(&format!("  {} {} [[attribute({})]];\n", gl_type_to_msl(&a.ty), a.name, i));
    }
    out.push_str("};\n");
    out.push_str("struct VOut {\n  float4 position [[position]];\n");
    for (i, v) in vary.iter().enumerate() {
        out.push_str(&format!("  {} {} [[user(v{})]];\n", gl_type_to_msl(&v.ty), v.name, i));
    }
    out.push_str("};\n");
    let uparam = if has_u { ", constant Uniforms& u [[buffer(1)]]" } else { "" };

    // vertex
    let mut vb = main_body(&vs);
    fix_trunc(&mut vb);
    type_fixups(&mut vb);
    builtin_fixups(&mut vb);
    sampler_fixups(&mut vb, &samps);
    for a in &attrs {
        wreplace(&mut vb, &a.name, &format!("in.{}", a.name));
    }
    for v in &vary {
        wreplace(&mut vb, &v.name, &format!("out.{}", v.name));
    }
    for u in &unis {
        wreplace(&mut vb, &u.name, &format!("u.{}", u.name));
    }
    wreplace(&mut vb, "gl_Position", "out.position");
    local_decl_fixups(&mut vb);
    out.push_str(&format!("vertex VOut vmain(VIn in [[stage_in]]{uparam}"));
    emit_samp_params(&mut out, &vb, &samps);
    out.push_str(&format!(") {{\n  VOut out;\n{vb}\n  return out;\n}}\n"));

    // fragment
    let mut fb = main_body(&fs);
    let frag_uses_coord = fb.contains("gl_FragCoord");
    fix_trunc(&mut fb);
    type_fixups(&mut fb);
    builtin_fixups(&mut fb);
    sampler_fixups(&mut fb, &samps);
    for v in &vary {
        wreplace(&mut fb, &v.name, &format!("in.{}", v.name));
    }
    for u in &unis {
        wreplace(&mut fb, &u.name, &format!("u.{}", u.name));
    }
    for fo in &fragouts {
        wreplace(&mut fb, &fo.name, "_frag");
    }
    wreplace(&mut fb, "gl_FragColor", "_frag");
    wreplace(&mut fb, "gl_FragCoord", "_dd_FragCoord");
    relational_fixups(&mut fb);
    local_decl_fixups(&mut fb);
    out.push_str("fragment float4 fmain(VOut in [[stage_in]]");
    out.push_str(uparam);
    emit_samp_params(&mut out, &fb, &samps);
    out.push_str(") {\n  float4 _frag = float4(0);\n");
    if frag_uses_coord {
        out.push_str("  float4 _dd_FragCoord = in.position;\n");
    }
    out.push_str(&format!("{fb}\n  return _frag;\n}}\n"));

    out
}
