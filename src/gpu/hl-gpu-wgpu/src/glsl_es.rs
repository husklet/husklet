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
//! `hl-gl/src/adapter/glsl.rs::emit_sampler_decls` and `service/frame.rs` already agree on, so the
//! driver's bind-group entries line up with the layout naga reflects here with no cross-side negotiation.

/// Suffix for the split texture / sampler halves of a combined sampler (shared with the driver's scheme).
const TEX_SUFFIX: &str = "_hltex";
const SMP_SUFFIX: &str = "_hlsmp";

/// Marker suffix stamped onto a `layout(location=L, index=1)` dual-source-blend output. naga's `glsl-in`
/// cannot parse the `index=` qualifier (and hardcodes `second_blend_source: false`), so [`normalize`] drops
/// the qualifier and renames the second-source output with this suffix; a module post-pass in `wgsl.rs`
/// then flips `second_blend_source` on the matching fragment-output member and strips the suffix again.
pub(crate) const BLEND_SRC1_SUFFIX: &str = "_hlbsrc1";

/// The desktop GLSL version naga's `glsl-in` accepts (ES profiles are rejected). We also seed
/// `__VERSION__` as a preprocessor define: naga's preprocessor (pp_rs) starts with *no* built-in defines,
/// so an unset `__VERSION__` evaluates to `0` inside `#if` expressions. GskGpu gates its `layout(binding)`
/// on `#if __VERSION__ < 420 …` (binding present only on the `#else`/desktop branch), so without this the
/// no-binding branch is taken and naga rejects the bindingless uniform block. Defining it to match the
/// pinned `#version` makes every `__VERSION__` conditional resolve as the desktop 460 it now is.
const DESKTOP_VERSION: &str = "#version 460\n#define __VERSION__ 460";

/// GLSL-ES sampler types this bring-up splits, mapped to the naga-accepted `(texture type, sampler type)`
/// pair. `samplerExternalOES` (ANGLE's YUV external image) is mapped to a plain 2D sampler for bring-up —
/// correct for the single-plane RGBA path; multi-plane YUV conversion is a later concern.
#[derive(Clone, Copy)]
enum SamplerType {
    TwoDimensional,
    External,
    Cube,
    TwoDimensionalArray,
    Shadow,
}

impl SamplerType {
    fn parse(source: &str) -> Option<Self> {
        Some(match source {
            "sampler2D" => Self::TwoDimensional,
            "samplerExternalOES" => Self::External,
            "samplerCube" => Self::Cube,
            "sampler2DArray" => Self::TwoDimensionalArray,
            "sampler2DShadow" => Self::Shadow,
            _ => return None,
        })
    }

    fn split(self) -> (&'static str, &'static str) {
        match self {
            Self::TwoDimensional | Self::External => ("texture2D", "sampler"),
            Self::Cube => ("textureCube", "sampler"),
            Self::TwoDimensionalArray => ("texture2DArray", "sampler"),
            Self::Shadow => ("texture2D", "samplerShadow"),
        }
    }

    /// The GLSL combining-constructor spelling that recombines a split `(texture, sampler)` pair back into the
    /// sampler value a texture built-in wants. It is the ORIGINAL combined sampler type — `sampler2D(t,s)`,
    /// `samplerCube(t,s)`, `sampler2DArray(t,s)`, `sampler2DShadow(t,s)` — NOT a hardcoded `sampler2D`, which
    /// naga rejects with `Unknown function 'sampler2D'` when the halves are cube/array/shadow typed.
    /// `samplerExternalOES` is sampled through the 2D path (single-plane RGBA), so it recombines as `sampler2D`.
    fn constructor(self) -> &'static str {
        match self {
            Self::TwoDimensional | Self::External => "sampler2D",
            Self::Cube => "samplerCube",
            Self::TwoDimensionalArray => "sampler2DArray",
            Self::Shadow => "sampler2DShadow",
        }
    }
}

/// GLSL texture built-ins: at a call to one of these, a sampler argument is recombined into a
/// `sampler2D(tex, smp)` expression; at any OTHER (user-defined) call it is passed as the two split args.
struct Identifier<'a>(&'a str);

impl Identifier<'_> {
    fn is_texture_builtin(&self) -> bool {
        matches!(
            self.0,
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

    fn is_keyword(&self) -> bool {
        matches!(
            self.0,
            "if" | "for" | "while" | "switch" | "return" | "do" | "else"
        )
    }
}

// ---------------------------------------------------------------------------------------------------
// Tokenizer — a minimal GLSL lexer sufficient for the structural edits below. Preprocessor lines and
// whitespace are preserved as opaque tokens so the rebuilt source stays byte-faithful outside the edits
// (naga runs its own preprocessor on the result, so `#define`/`#ifdef` must survive untouched).
// ---------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String), // identifier or number (only identifiers are ever matched semantically)
    Punct(char),  // any single non-word, non-space character
    Ws(String),   // a run of whitespace
    Pp(String),   // a whole preprocessor line, leading `#` … through end-of-line (no trailing '\n')
}

impl Tok {
    fn is_significant(&self) -> bool {
        !matches!(self, Self::Ws(_))
    }
}

struct Tokens(Vec<Tok>);

impl Tokens {
    /// Strip comments and lex source while preserving whitespace and preprocessor lines.
    fn from_source(source: &str) -> Self {
        let source = Source::without_comments(source);
        let bytes = source.as_bytes();
        let mut tokens = Vec::new();
        let mut index = 0;
        let mut at_line_start = true;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'#' && at_line_start {
                let start = index;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                tokens.push(Tok::Pp(source[start..index].to_string()));
                continue;
            }
            if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
                let start = index;
                while index < bytes.len() && matches!(bytes[index], b' ' | b'\t' | b'\r' | b'\n') {
                    if bytes[index] == b'\n' {
                        at_line_start = true;
                    }
                    index += 1;
                }
                tokens.push(Tok::Ws(source[start..index].to_string()));
                continue;
            }
            at_line_start = false;
            if byte == b'_' || byte.is_ascii_alphabetic() || byte.is_ascii_digit() {
                let start = index;
                while index < bytes.len()
                    && (bytes[index] == b'_'
                        || bytes[index].is_ascii_alphanumeric()
                        || bytes[index] == b'.')
                {
                    if bytes[index] == b'.' {
                        let previous_is_digit = index > start && bytes[index - 1].is_ascii_digit();
                        let next_is_digit =
                            index + 1 < bytes.len() && bytes[index + 1].is_ascii_digit();
                        if !(previous_is_digit || next_is_digit) {
                            break;
                        }
                    }
                    index += 1;
                }
                tokens.push(Tok::Word(source[start..index].to_string()));
                continue;
            }
            tokens.push(Tok::Punct(byte as char));
            index += 1;
        }
        Self(tokens)
    }
}

impl std::ops::Deref for Tokens {
    type Target = [Tok];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Tokens {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for Tokens {
    type Item = Tok;
    type IntoIter = std::vec::IntoIter<Tok>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

trait TokenSlice {
    fn source(&self) -> String;
    fn next_significant(&self, index: usize) -> Option<usize>;
    fn output_name(&self, layout_end: usize) -> Option<String>;
    fn main_body(&self, name: usize) -> Option<usize>;
}

impl TokenSlice for [Tok] {
    fn source(&self) -> String {
        let mut source = String::new();
        for token in self {
            match token {
                Tok::Word(word) => source.push_str(word),
                Tok::Punct(punctuation) => source.push(*punctuation),
                Tok::Ws(whitespace) => source.push_str(whitespace),
                Tok::Pp(preprocessor) => source.push_str(preprocessor),
            }
        }
        source
    }

    fn next_significant(&self, index: usize) -> Option<usize> {
        (index..self.len()).find(|&candidate| self[candidate].is_significant())
    }

    fn output_name(&self, layout_end: usize) -> Option<String> {
        let mut index = self.next_significant(layout_end + 1)?;
        while matches!(&self[index], Tok::Word(word) if matches!(word.as_str(), "flat" | "centroid" | "smooth" | "noperspective"))
        {
            index = self.next_significant(index + 1)?;
        }
        if !matches!(&self[index], Tok::Word(word) if word == "out") {
            return None;
        }
        let ty = self.next_significant(index + 1)?;
        let name = self.next_significant(ty + 1)?;
        match &self[name] {
            Tok::Word(name) => Some(name.clone()),
            _ => None,
        }
    }

    fn main_body(&self, name: usize) -> Option<usize> {
        let left = self.next_significant(name + 1)?;
        if self[left] != Tok::Punct('(') {
            return None;
        }
        let right = match_close(self, left, '(', ')');
        if right >= self.len() {
            return None;
        }
        let body = self.next_significant(right + 1)?;
        (self[body] == Tok::Punct('{')).then_some(body)
    }
}

// ---------------------------------------------------------------------------------------------------
// isinf() rewrite (naga wgsl-out has no IsInf emitter)
// ---------------------------------------------------------------------------------------------------

/// The largest finite f32, as a GLSL float literal. A value's magnitude exceeds it iff the value is ±∞.
const F32_FINITE_MAX_LIT: &str = "3.40282347e38";

/// Rewrite every `isinf(x)` call to `(abs(x) > 3.40282347e38)` — a finite-max-bound test that survives
/// naga's `wgsl-out`, which has NO `IsInf` emitter and otherwise NACKs the whole shader with
/// `wgsl-out: Unsupported relational function: IsInf`. naga's `glsl-in` accepts `isinf` and lowers it to
/// a `RelationalFunction::IsInf` expression; the crash is purely in the WGSL writer, so removing the call
/// textually before parsing is the surgical fix.
///
/// Exactness (scalar f32, the shape Chrome's GLES shaders use): both `+∞` and `-∞` have `abs(x) == ∞ >
/// FLT_MAX`; every finite value has `abs(x) <= FLT_MAX`; and `abs(NaN) > FLT_MAX` is `false`, matching
/// `isinf(NaN) == false`. So the rewrite is bit-exact with `isinf` for every scalar float input.
///
/// A fast path leaves any shader WITHOUT `isinf` byte-for-byte untouched (no tokenize/strip). Comments are
/// stripped only for a shader that does contain `isinf`, so an `isinf(` inside a comment cannot be
/// rewritten and cannot leave a dangling fragment. Nested `isinf` in the argument is handled recursively.
pub(crate) struct Source<'a> {
    text: &'a str,
}

impl<'a> Source<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub(crate) fn rewrite_isinf(&self) -> String {
        Self::rewrite(self.text)
    }

    fn rewrite(text: &str) -> String {
        if !text.contains("isinf") {
            return text.to_string();
        }
        let s = Self::without_comments(text);
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len() + 32);
        let mut i = 0usize;
        let mut flush_from = 0usize;
        while i < bytes.len() {
            let on_boundary = i == 0
                || !(bytes[i - 1] == b'_'
                    || bytes[i - 1].is_ascii_alphanumeric()
                    || bytes[i - 1] == b'.');
            let followed_by_word = bytes
                .get(i + 5)
                .is_some_and(|&byte| byte == b'_' || byte.is_ascii_alphanumeric() || byte == b'.');
            if on_boundary && !followed_by_word && s[i..].starts_with("isinf") {
                // Skip any whitespace between `isinf` and its opening paren.
                let mut j = i + 5;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'(') {
                    // Find the paren that closes this call.
                    let mut depth = 0i32;
                    let mut k = j;
                    let close = loop {
                        match bytes.get(k) {
                            None => break bytes.len(),
                            Some(b'(') => depth += 1,
                            Some(b')') => {
                                depth -= 1;
                                if depth == 0 {
                                    break k;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    };
                    if close < bytes.len() {
                        out.push_str(&s[flush_from..i]);
                        let inner = Self::rewrite(&s[j + 1..close]);
                        out.push_str("(abs(");
                        out.push_str(inner.trim());
                        out.push_str(") > ");
                        out.push_str(F32_FINITE_MAX_LIT);
                        out.push(')');
                        i = close + 1;
                        flush_from = i;
                        continue;
                    }
                }
            }
            i += 1;
        }
        out.push_str(&s[flush_from..]);
        out
    }

    // ---------------------------------------------------------------------------------------------------
    // Detection
    // ---------------------------------------------------------------------------------------------------

    /// Whether `src` is GLSL-ES / GskGpu-shaped and must take this transform rather than naga's direct
    /// `glsl-in`. True when it declares an ES `#version … es`, uses an ES-only builtin, or carries a combined
    /// `sampler2D`/external sampler that naga's `glsl-in` cannot parse. The simple ES2 conformance shaders the
    /// GL driver already rewrites to desktop form (separate `texture2D`+`sampler`, `#version 460`) match none
    /// of these, so they keep the existing direct path unchanged.
    pub(crate) fn is_es(&self) -> bool {
        for line in self.text.lines() {
            let t = line.trim_start();
            if t.starts_with("#version") && t.trim_end().ends_with("es") {
                return true;
            }
        }
        if self.text.contains("gl_VertexID") || self.text.contains("gl_InstanceID") {
            return true;
        }
        // A combined sampler global/parameter (a sampler type NOT immediately followed by `(`, i.e. not the
        // `sampler2D(tex,smp)` constructor) is the unmistakable GskGpu/ANGLE marker naga cannot handle.
        let toks = Tokens::from_source(self.text);
        for (k, t) in toks.iter().enumerate() {
            if let Tok::Word(w) = t {
                if SamplerType::parse(w).is_some() {
                    if let Some(j) = toks.next_significant(k + 1) {
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
    /// true. Returns the rewritten source; the caller feeds it to naga's `glsl-in`. `stage` selects the
    /// direction of GskGpu's `PASS`/`PASS_FLAT` varyings (out in the vertex stage, in in the fragment stage)
    /// when an aggregate interface member has to be split into per-location vectors.
    pub(crate) fn normalize(&self, stage: naga::ShaderStage) -> String {
        let mut toks = Tokens::from_source(self.text);

        toks.normalize_directives_and_precision();
        toks.normalize_dual_source();
        toks.split_std140_mat2();
        toks.split_aggregate_io(stage);
        let mut sampler_types = toks.split_global_samplers();
        for p in toks.split_param_samplers() {
            if !sampler_types.iter().any(|(n, _)| *n == p.0) {
                sampler_types.push(p);
            }
        }
        toks.map_es_texture_builtins();
        toks.recombine_sampler_uses(&sampler_types);
        let toks = SwitchRewrite::lower_all(&toks);

        toks.as_slice().source()
    }

    fn without_comments(source: &str) -> String {
        let bytes = source.as_bytes();
        let mut output = String::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'/' {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            } else if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            } else {
                output.push(bytes[index] as char);
                index += 1;
            }
        }
        output
    }
}

/// One `case`/`default` group of a lowered switch: the label constant expressions it matches (empty for a
/// pure `default`), whether it is the `default`, and its statement body as source text.
mod io;
mod preprocessor;
mod std140;
mod switch;

use switch::{match_close, SwitchRewrite};

#[cfg(test)]
mod tests;
