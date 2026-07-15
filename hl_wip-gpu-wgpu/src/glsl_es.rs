//! GLSL-ES → naga-acceptable desktop GLSL, in pure Rust (no glslang/shaderc host dependency).
//!
//! # Why this exists
//!
//! GTK's GskGpu "gl" renderer and Chrome's ANGLE both emit *GLSL-ES* that naga-24's `glsl-in` cannot
//! consume directly. The robust industry route is glslang/shaderc (host C++), but that toolchain is not
//! available offline here (no cmake, no vendored glslang sources, no prebuilt libshaderc). So this module
//! reproduces the *only* naga-relevant parts of that lowering as a textual transform, sized to the shapes
//! GskGpu/ANGLE actually emit, then hands the result to the SAME `glsl-in → wgsl-out` path the simple ES2
//! conformance shaders use.
//!
//! # What naga-24 glsl-in rejects, and how we fix it (each measured against the vendored naga)
//!
//! | GLSL-ES construct                        | naga verdict            | transform                                   |
//! |------------------------------------------|-------------------------|---------------------------------------------|
//! | `#version 320 es` / `es` profile         | InvalidVersion/Profile  | pin `#version 460`                          |
//! | `gl_VertexID` / `gl_InstanceID`          | UnknownVariable         | → `gl_VertexIndex` / `gl_InstanceIndex`     |
//! | `precision`/`highp`/`mediump`/`lowp`     | invalid in core         | strip                                       |
//! | `uniform sampler2D t;` (combined global) | NotImplemented          | → `texture2D t_hltex;` + `sampler t_hlsmp;` |
//! | `vec4 f(sampler2D tex, …)` (combined arg)| NotImplemented          | → `f(texture2D tex_hltex, sampler tex_hlsmp,…)` |
//! | `texture(t, uv)` on a combined name      | (name now split)        | → `texture(sampler2D(t_hltex, t_hlsmp), uv)`|
//!
//! The keystone (verified against naga-24, `tests/gskgpu_glsl.rs`): naga *accepts* the SEPARATE
//! `texture2D` + `sampler` model **as function parameters** recombined inside a helper by the
//! `sampler2D(tex, smp)` constructor — while it rejects the combined `sampler2D` as a global OR as a
//! parameter type. So a sampler that crosses a helper's signature (GskGpu's `vec4 gsk_texture(sampler2D
//! tex, vec2 p)`) is split into a `(texture2D, sampler)` PAIR everywhere: the pair is passed as two
//! arguments at a user-function call, and recombined with `sampler2D(…)` only at a texture-builtin call.
//! That context distinction (builtin vs. user function) is exactly what the recombine pass tracks.
//!
//! # Binding coordination
//!
//! Samplers are numbered in declaration order `k`; the pair lands at texture binding `1 + 2k` and sampler
//! binding `2 + 2k`, with the uniform block reserved at binding 0. This is the SAME scheme
//! `hl_wip-gl/src/adapter/glsl.rs::emit_sampler_decls` and `service/frame.rs` already agree on, so the
//! driver's bind-group entries line up with the layout naga reflects here with no cross-side negotiation.

/// Suffix for the split texture / sampler halves of a combined sampler (shared with the driver's scheme).
const TEX_SUFFIX: &str = "_hltex";
const SMP_SUFFIX: &str = "_hlsmp";

/// The desktop GLSL version naga's `glsl-in` accepts (ES profiles are rejected).
const DESKTOP_VERSION: &str = "#version 460\n";

/// GLSL-ES sampler types this bring-up splits, mapped to the naga-accepted `(texture type, sampler type)`
/// pair. `samplerExternalOES` (ANGLE's YUV external image) is mapped to a plain 2D sampler for bring-up —
/// correct for the single-plane RGBA path; multi-plane YUV conversion is a later concern.
fn split_sampler_ty(ty: &str) -> Option<(&'static str, &'static str)> {
    Some(match ty {
        "sampler2D" | "samplerExternalOES" => ("texture2D", "sampler"),
        "samplerCube" => ("textureCube", "sampler"),
        "sampler2DArray" => ("texture2DArray", "sampler"),
        "sampler2DShadow" => ("texture2D", "samplerShadow"),
        _ => return None,
    })
}

fn is_sampler_ty(ty: &str) -> bool {
    split_sampler_ty(ty).is_some()
}

/// GLSL texture built-ins: at a call to one of these, a sampler argument is recombined into a
/// `sampler2D(tex, smp)` expression; at any OTHER (user-defined) call it is passed as the two split args.
fn is_texture_builtin(name: &str) -> bool {
    matches!(
        name,
        "texture"
            | "textureLod"
            | "textureProj"
            | "textureProjLod"
            | "textureGrad"
            | "textureGradOffset"
            | "textureOffset"
            | "textureLodOffset"
            | "textureProjOffset"
            | "texelFetch"
            | "texelFetchOffset"
            | "textureSize"
            | "textureGather"
            | "textureGatherOffset"
            | "textureQueryLod"
            | "textureQueryLevels"
            // ES1/ES2 spellings we normalize to `texture(` but guard here too.
            | "texture2D"
            | "texture2DProj"
            | "texture2DLod"
            | "textureCube"
            | "textureCubeLod"
    )
}

fn is_keyword(word: &str) -> bool {
    matches!(word, "if" | "for" | "while" | "switch" | "return" | "do" | "else")
}

// ---------------------------------------------------------------------------------------------------
// Tokenizer — a minimal GLSL lexer sufficient for the structural edits below. Preprocessor lines and
// whitespace are preserved as opaque tokens so the rebuilt source stays byte-faithful outside the edits
// (naga runs its own preprocessor on the result, so `#define`/`#ifdef` must survive untouched).
// ---------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),  // identifier or number (only identifiers are ever matched semantically)
    Punct(char),   // any single non-word, non-space character
    Ws(String),    // a run of whitespace
    Pp(String),    // a whole preprocessor line, leading `#` … through end-of-line (no trailing '\n')
}

fn is_word_byte(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric() || c == b'.'
}

/// Strip `//` and `/* */` comments, then tokenize. Comments are removed first so a `sampler2D` inside a
/// comment never trips the structural passes.
fn tokenize(src: &str) -> Vec<Tok> {
    let s = strip_comments(src);
    let b = s.as_bytes();
    let n = b.len();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut at_line_start = true;
    while i < n {
        let c = b[i];
        if c == b'#' && at_line_start {
            let start = i;
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            toks.push(Tok::Pp(s[start..i].to_string()));
            // leave the '\n' to be picked up as whitespace below
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
            let start = i;
            while i < n && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
                if b[i] == b'\n' {
                    at_line_start = true;
                }
                i += 1;
            }
            toks.push(Tok::Ws(s[start..i].to_string()));
            continue;
        }
        at_line_start = false;
        if c == b'_' || c.is_ascii_alphabetic() || c.is_ascii_digit() {
            let start = i;
            // A number like `1.0` keeps its '.'; an identifier never contains '.', so treat a '.' that is
            // NOT between digits as a token boundary (member access `push.mvp`).
            while i < n && is_word_byte(b[i]) {
                if b[i] == b'.' {
                    let prev_digit = i > start && b[i - 1].is_ascii_digit();
                    let next_digit = i + 1 < n && b[i + 1].is_ascii_digit();
                    if !(prev_digit || next_digit) {
                        break;
                    }
                }
                i += 1;
            }
            toks.push(Tok::Word(s[start..i].to_string()));
            continue;
        }
        toks.push(Tok::Punct(c as char));
        i += 1;
    }
    toks
}

fn detok(toks: &[Tok]) -> String {
    let mut out = String::new();
    for t in toks {
        match t {
            Tok::Word(w) => out.push_str(w),
            Tok::Punct(c) => out.push(*c),
            Tok::Ws(w) => out.push_str(w),
            Tok::Pp(p) => {
                out.push_str(p);
            }
        }
    }
    out
}

/// Index of the next non-whitespace token at or after `i` (whitespace and comments already separated).
fn next_significant(toks: &[Tok], i: usize) -> Option<usize> {
    (i..toks.len()).find(|&j| !matches!(toks[j], Tok::Ws(_)))
}

// ---------------------------------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------------------------------

/// Whether `src` is GLSL-ES / GskGpu-shaped and must take this transform rather than naga's direct
/// `glsl-in`. True when it declares an ES `#version … es`, uses an ES-only builtin, or carries a combined
/// `sampler2D`/external sampler that naga's `glsl-in` cannot parse. The simple ES2 conformance shaders the
/// GL driver already rewrites to desktop form (separate `texture2D`+`sampler`, `#version 460`) match none
/// of these, so they keep the existing direct path unchanged.
pub fn is_es_glsl(src: &str) -> bool {
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("#version") && t.trim_end().ends_with("es") {
            return true;
        }
    }
    if src.contains("gl_VertexID") || src.contains("gl_InstanceID") {
        return true;
    }
    // A combined sampler global/parameter (a sampler type NOT immediately followed by `(`, i.e. not the
    // `sampler2D(tex,smp)` constructor) is the unmistakable GskGpu/ANGLE marker naga cannot handle.
    let toks = tokenize(src);
    for (k, t) in toks.iter().enumerate() {
        if let Tok::Word(w) = t {
            if is_sampler_ty(w) {
                if let Some(j) = next_significant(&toks, k + 1) {
                    if toks[j] != Tok::Punct('(') {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------------
// The transform
// ---------------------------------------------------------------------------------------------------

/// Normalize GLSL-ES source into the naga-acceptable desktop GLSL the existing `glsl_to_wgsl` path
/// compiles. Idempotent enough to run on already-desktop source, but only invoked when [`is_es_glsl`] is
/// true. Returns the rewritten source; the caller feeds it to naga's `glsl-in`.
pub fn normalize(src: &str) -> String {
    let mut toks = tokenize(src);

    normalize_directives_and_precision(&mut toks);
    let mut sampler_names = split_global_samplers(&mut toks);
    for p in split_param_samplers(&mut toks) {
        if !sampler_names.contains(&p) {
            sampler_names.push(p);
        }
    }
    map_es_texture_builtins(&mut toks);
    recombine_sampler_uses(&mut toks, &sampler_names);

    detok(&toks)
}

/// `#version … [es]` → `#version 460`; `gl_VertexID`/`gl_InstanceID` → the naga builtins; drop `precision
/// …;` statements and inline `highp`/`mediump`/`lowp` qualifiers.
fn normalize_directives_and_precision(toks: &mut Vec<Tok>) {
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Pp(p) if p.trim_start().starts_with("#version") => {
                out.push(Tok::Pp(DESKTOP_VERSION.trim_end().to_string()));
            }
            // naga's `gl_VertexIndex`/`gl_InstanceIndex` builtins are `uint`, but the GLES `gl_VertexID`/
            // `gl_InstanceID` they replace are `int`; wrap in an `int(…)` cast so a `int id = gl_VertexID;`
            // assignment keeps matching store types (naga validates this).
            Tok::Word(w) if w == "gl_VertexID" => out.push(Tok::Word("int(gl_VertexIndex)".into())),
            Tok::Word(w) if w == "gl_InstanceID" => {
                out.push(Tok::Word("int(gl_InstanceIndex)".into()))
            }
            Tok::Word(w) if w == "highp" || w == "mediump" || w == "lowp" => {
                // Drop the qualifier; also drop a following separating space so we don't leave a double gap.
                if matches!(toks.get(i + 1), Some(Tok::Ws(_))) {
                    i += 1;
                }
            }
            Tok::Word(w) if w == "precision" => {
                // `precision <qual> <type> ;` — a whole statement with no runtime effect in core GLSL.
                while i < toks.len() && toks[i] != Tok::Punct(';') {
                    i += 1;
                }
                // skip the ';' too (loop's i += 1 below advances past it)
            }
            other => out.push(other.clone()),
        }
        i += 1;
    }
    *toks = out;
}

/// Rewrite every `uniform <samplerType> NAME;` global into a separate `texture`/`sampler` pair at
/// coordinated bindings (uniform block reserved at 0; sampler `k` → texture `1+2k`, sampler `2+2k`),
/// returning the sampler names in declaration order.
fn split_global_samplers(toks: &mut Vec<Tok>) -> Vec<String> {
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if matches!(&toks[i], Tok::Word(w) if w == "uniform") {
            // Look ahead: uniform <ws> <samplerType> <ws> NAME <ws?> [ [ .. ] ] ;
            if let Some(ty_idx) = next_significant(toks, i + 1) {
                if let Tok::Word(ty) = &toks[ty_idx] {
                    if let Some((tex_ty, smp_ty)) = split_sampler_ty(ty) {
                        if let Some(name_idx) = next_significant(toks, ty_idx + 1) {
                            if let Tok::Word(name) = toks[name_idx].clone() {
                                // find the terminating ';'
                                let mut j = name_idx + 1;
                                while j < toks.len() && toks[j] != Tok::Punct(';') {
                                    j += 1;
                                }
                                if j < toks.len() {
                                    let k = names.len();
                                    let (tex_b, smp_b) = (1 + 2 * k, 2 + 2 * k);
                                    out.push(Tok::Pp(format!(
                                        "layout(binding = {tex_b}) uniform {tex_ty} {name}{TEX_SUFFIX};\nlayout(binding = {smp_b}) uniform {smp_ty} {name}{SMP_SUFFIX};"
                                    )));
                                    names.push(name);
                                    i = j + 1; // resume after ';'
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    *toks = out;
    names
}

/// Rewrite every `<samplerType> NAME` *function parameter* into the two split params
/// `<tex> NAME_hltex, <smp> NAME_hlsmp`, returning the parameter names split. Runs after
/// [`split_global_samplers`] has consumed the `uniform …` globals, so any remaining sampler-typed word is
/// a parameter (GLSL-ES has no local sampler variables and the `sampler2D(…)` constructor is not emitted
/// yet).
fn split_param_samplers(toks: &mut Vec<Tok>) -> Vec<String> {
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Tok::Word(ty) = &toks[i] {
            if let Some((tex_ty, smp_ty)) = split_sampler_ty(ty) {
                if let Some(name_idx) = next_significant(toks, i + 1) {
                    if let Tok::Word(name) = &toks[name_idx].clone() {
                        out.push(Tok::Word(format!("{tex_ty} {name}{TEX_SUFFIX}, {smp_ty} {name}{SMP_SUFFIX}")));
                        names.push(name.clone());
                        i = name_idx + 1;
                        continue;
                    }
                }
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    *toks = out;
    names
}

/// Normalize ES `texture2D(`/`textureCube(` builtin spellings to the desktop `texture(` (the sampler
/// argument recombination in [`recombine_sampler_uses`] then supplies a valid `sampler2D(…)` first arg).
fn map_es_texture_builtins(toks: &mut [Tok]) {
    for k in 0..toks.len() {
        let mapped = match &toks[k] {
            Tok::Word(w) => match w.as_str() {
                "texture2D" | "textureCube" | "texture2DProj" | "texture2DLod"
                | "textureCubeLod" => Some("texture"),
                _ => None,
            },
            _ => None,
        };
        if let Some(m) = mapped {
            // only when actually a call: next significant token is '('
            if next_significant(toks, k + 1).map(|j| toks[j] == Tok::Punct('(')) == Some(true) {
                toks[k] = Tok::Word(m.to_string());
            }
        }
    }
}

/// Replace each *use* of a split sampler name with either the recombined `sampler2D(NAME_hltex,
/// NAME_hlsmp)` expression (inside a texture built-in call) or the two split arguments `NAME_hltex,
/// NAME_hlsmp` (inside a user-function call), tracking the enclosing call while scanning.
fn recombine_sampler_uses(toks: &mut Vec<Tok>, sampler_names: &[String]) {
    // Enclosing-call stack: each `(` pushes whether its call is a texture built-in (`Some(true)`), a user
    // call (`Some(false)`), or a grouping/keyword paren (`None`).
    let mut stack: Vec<Option<bool>> = Vec::new();
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut last_word: Option<String> = None;

    for k in 0..toks.len() {
        match &toks[k] {
            Tok::Punct('(') => {
                let ctx = match &last_word {
                    Some(w) if is_keyword(w) => None,
                    Some(w) => Some(is_texture_builtin(w)),
                    None => None,
                };
                stack.push(ctx);
                out.push(toks[k].clone());
                last_word = None;
            }
            Tok::Punct(')') => {
                stack.pop();
                out.push(toks[k].clone());
                last_word = None;
            }
            Tok::Word(w) if sampler_names.iter().any(|s| s == w) => {
                // A sampler value use. Nearest enclosing CALL determines the expansion.
                let builtin = stack.iter().rev().find_map(|c| *c).unwrap_or(true);
                let expansion = if builtin {
                    format!("sampler2D({w}{TEX_SUFFIX}, {w}{SMP_SUFFIX})")
                } else {
                    format!("{w}{TEX_SUFFIX}, {w}{SMP_SUFFIX}")
                };
                out.push(Tok::Word(expansion));
                last_word = None;
            }
            Tok::Word(w) => {
                last_word = Some(w.clone());
                out.push(toks[k].clone());
            }
            Tok::Ws(_) => out.push(toks[k].clone()),
            other => {
                last_word = None;
                out.push(other.clone());
            }
        }
    }
    *toks = out;
}

// ---------------------------------------------------------------------------------------------------
// Comment stripping (kept local so tokenization is self-contained)
// ---------------------------------------------------------------------------------------------------

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
            r = (r + 2).min(n);
        } else {
            out.push(b[r] as char);
            r += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_es_and_combined_sampler_but_not_desktop() {
        assert!(is_es_glsl("#version 320 es\nvoid main(){}"));
        assert!(is_es_glsl("#version 300 es\nuniform sampler2D t;"));
        assert!(is_es_glsl("uniform sampler2D t; void main(){}"));
        assert!(is_es_glsl("void main(){ int i = gl_VertexID; }"));
        // Already-desktop split source (what the ES2 driver path emits) must NOT be re-taken.
        let desktop = "#version 460\nlayout(binding=1) uniform texture2D t_hltex;\nlayout(binding=2) uniform sampler t_hlsmp;\nvoid main(){ vec4 c = texture(sampler2D(t_hltex,t_hlsmp), vec2(0.0)); }";
        assert!(!is_es_glsl(desktop), "desktop split source must keep the direct path");
    }

    #[test]
    fn splits_global_sampler_and_recombines_at_builtin() {
        let src = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 uv;\nout vec4 c;\nvoid main(){ c = texture(uTex, uv); }";
        let out = normalize(src);
        assert!(out.contains("#version 460"), "{out}");
        assert!(!out.contains("320 es"), "{out}");
        assert!(!out.contains("precision"), "precision stripped: {out}");
        assert!(out.contains("uniform texture2D uTex_hltex"), "{out}");
        assert!(out.contains("uniform sampler uTex_hlsmp"), "{out}");
        assert!(out.contains("texture(sampler2D(uTex_hltex, uTex_hlsmp), uv)"), "{out}");
    }

    #[test]
    fn splits_sampler_function_parameter_and_call_site() {
        let src = "#version 320 es\nuniform sampler2D uTex;\nvec4 fetch(sampler2D tex, vec2 p){ return texture(tex, p); }\nvoid main(){ gl_Position = fetch(uTex, vec2(0.0)); }";
        let out = normalize(src);
        // parameter split into two
        assert!(out.contains("texture2D tex_hltex, sampler tex_hlsmp"), "param split: {out}");
        // recombine inside helper (texture builtin)
        assert!(out.contains("texture(sampler2D(tex_hltex, tex_hlsmp), p)"), "helper recombine: {out}");
        // pass split pair at the user-function call site
        assert!(out.contains("fetch(uTex_hltex, uTex_hlsmp, vec2(0.0))"), "call-site pair: {out}");
    }
}
