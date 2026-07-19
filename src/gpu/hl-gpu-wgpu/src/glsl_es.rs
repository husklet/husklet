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
struct SwitchGroup {
    values: Vec<String>,
    is_default: bool,
    body: String,
}

/// A lowered switch and the token index just past its closing `}`.
struct SwitchRewrite {
    text: String,
    end: usize,
}

/// Index of the `Punct(close)` matching the `Punct(open)` at `open_idx`, counting nesting; `toks.len()` if
/// unbalanced. Only `Tok::Punct` participates — punctuation embedded inside a merged `Tok::Word` (e.g. the
/// `sampler2D(a, b)` recombination) is opaque and always balanced, so it never disturbs the count.
fn match_close(toks: &[Tok], open_idx: usize, open: char, close: char) -> usize {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().skip(open_idx) {
        if let Tok::Punct(c) = t {
            if *c == open {
                depth += 1;
            } else if *c == close {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
        }
    }
    toks.len()
}

/// Index of the first top-level `:` (paren/brace depth 0) at or after `start`, ending a `case`/`default`
/// label; `None` if absent.
impl SwitchGroup {
    fn top_colon(toks: &[Tok], start: usize) -> Option<usize> {
        let (mut p, mut b) = (0i32, 0i32);
        for (i, t) in toks.iter().enumerate().skip(start) {
            match t {
                Tok::Punct('(') => p += 1,
                Tok::Punct(')') => p -= 1,
                Tok::Punct('{') => b += 1,
                Tok::Punct('}') => b -= 1,
                Tok::Punct(':') if p == 0 && b == 0 => return Some(i),
                _ => {}
            }
        }
        None
    }

    /// Split a switch body's tokens into `case`/`default` groups, merging stacked (empty-body) labels into the
    /// following group. Returns `None` only if a label is missing its `:`.
    fn parse(body: &[Tok]) -> Option<Vec<Self>> {
        let mut groups: Vec<Self> = Vec::new();
        let mut cur_values: Vec<String> = Vec::new();
        let mut cur_default = false;
        let mut cur_body: Vec<Tok> = Vec::new();
        let (mut paren, mut brace) = (0i32, 0i32);
        let mut i = 0;
        while i < body.len() {
            let at_top = paren == 0 && brace == 0;
            if at_top {
                if let Tok::Word(w) = &body[i] {
                    if w == "case" || w == "default" {
                        if cur_body.iter().any(Tok::is_significant) {
                            groups.push(Self {
                                values: std::mem::take(&mut cur_values),
                                is_default: std::mem::take(&mut cur_default),
                                body: Self::body_without_break(&cur_body),
                            });
                            cur_body.clear();
                        }
                        let colon = Self::top_colon(body, i + 1)?;
                        if w == "default" {
                            cur_default = true;
                        } else {
                            cur_values.push(body[i + 1..colon].source());
                        }
                        i = colon + 1;
                        continue;
                    }
                }
            }
            if let Tok::Punct(c) = &body[i] {
                match c {
                    '(' => paren += 1,
                    ')' => paren -= 1,
                    '{' => brace += 1,
                    '}' => brace -= 1,
                    _ => {}
                }
            }
            cur_body.push(body[i].clone());
            i += 1;
        }
        if cur_default || !cur_values.is_empty() || cur_body.iter().any(Tok::is_significant) {
            groups.push(Self {
                values: cur_values,
                is_default: cur_default,
                body: Self::body_without_break(&cur_body),
            });
        }
        Some(groups)
    }

    fn body_without_break(body: &[Tok]) -> String {
        let sig: Vec<usize> = (0..body.len())
            .filter(|&j| body[j].is_significant())
            .collect();
        if sig.len() >= 2 {
            let last = sig[sig.len() - 1];
            let prev = sig[sig.len() - 2];
            if matches!(&body[last], Tok::Punct(';'))
                && matches!(&body[prev], Tok::Word(w) if w == "break")
            {
                let kept: Vec<Tok> = body
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != last && *j != prev)
                    .map(|(_, t)| t.clone())
                    .collect();
                return kept.as_slice().source();
            }
        }
        body.source()
    }
}

/// Lower every GLSL `switch` statement to an equivalent `if / else if / else` chain.
///
/// naga's `glsl-in` marks a `switch` case as `fall_through` unless it ends in a *literal* `break`: its
/// case-terminator is recorded with `get_or_insert` (`front/glsl/parser/functions.rs`), so a case whose
/// first terminator is a `return` — every GskGpu color-state / slice `switch` — stays fall-through, and
/// `wgsl-out` then rejects the non-empty fall-through case (`back/wgsl/writer.rs`). GskGpu never falls
/// through a *non-empty* case (each ends in `return`), so an if/else chain is semantically identical and
/// sidesteps naga's switch modeling entirely. The selector is a side-effect-free expression (`cs`,
/// `slice`, …), so it is repeated in each equality test rather than spilled to a temporary whose type we
/// would have to infer. Stacked (empty) labels OR into the following case's condition; `default` (wherever
/// it appears) becomes the trailing `else`. Nested switches are lowered first.
impl SwitchRewrite {
    fn lower_all(toks: &[Tok]) -> Vec<Tok> {
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "switch") {
                if let Some(rep) = Self::from_tokens(toks, i) {
                    out.extend(Tokens::from_source(&rep.text));
                    i = rep.end;
                    continue;
                }
            }
            out.push(toks[i].clone());
            i += 1;
        }
        out
    }

    /// Attempt to lower the `switch` whose keyword is at `switch_idx`. Returns `None` (leaving the switch
    /// untouched) if the shape is not the expected `switch (SELECTOR) { … }`.
    fn from_tokens(toks: &[Tok], switch_idx: usize) -> Option<Self> {
        let lp = toks.next_significant(switch_idx + 1)?;
        if toks[lp] != Tok::Punct('(') {
            return None;
        }
        let rp = match_close(toks, lp, '(', ')');
        if rp >= toks.len() {
            return None;
        }
        let lb = toks.next_significant(rp + 1)?;
        if toks[lb] != Tok::Punct('{') {
            return None;
        }
        let rb = match_close(toks, lb, '{', '}');
        if rb >= toks.len() {
            return None;
        }

        let selector = toks[lp + 1..rp].source();
        let selector = selector.trim();
        // Lower any nested switches in the body before parsing this one, so inner `case`s are gone.
        let body = Self::lower_all(&toks[lb + 1..rb]);
        let groups = SwitchGroup::parse(&body)?;

        let mut nondefault: Vec<&SwitchGroup> = Vec::new();
        let mut default_body: Option<&str> = None;
        for g in &groups {
            if g.body.trim().is_empty() && !g.is_default {
                continue; // a degenerate empty non-default group matches nothing meaningful
            }
            if g.is_default {
                default_body = Some(g.body.as_str());
            } else {
                nondefault.push(g);
            }
        }

        let mut text = String::from("{\n");
        for (k, g) in nondefault.iter().enumerate() {
            let cond = g
                .values
                .iter()
                .map(|v| format!("(({selector}) == ({}))", v.trim()))
                .collect::<Vec<_>>()
                .join(" || ");
            let kw = if k == 0 { "if" } else { "else if" };
            text.push_str(&format!("{kw} ({cond}) {{\n{}\n}}\n", g.body));
        }
        if let Some(db) = default_body {
            if nondefault.is_empty() {
                text.push_str(&format!("{{\n{db}\n}}\n"));
            } else {
                text.push_str(&format!("else {{\n{db}\n}}\n"));
            }
        }
        text.push_str("}\n");
        Some(Self { text, end: rb + 1 })
    }
}

/// `#version … [es]` → `#version 460`; `gl_VertexID`/`gl_InstanceID` → the naga builtins; drop `precision
/// …;` statements and inline `highp`/`mediump`/`lowp` qualifiers.
impl Tokens {
    fn normalize_directives_and_precision(&mut self) {
        let toks = &mut self.0;
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            match &toks[i] {
                Tok::Pp(p) if p.trim_start().starts_with("#version") => {
                    out.push(Tok::Pp(DESKTOP_VERSION.trim_end().to_string()));
                }
                // Preprocessor lines are opaque tokens, but GskGpu hides the vertex-index builtin inside a
                // `#define GSK_VERTEX_INDEX gl_VertexID` macro *body*: naga's preprocessor expands that macro at
                // every use site, so the raw `gl_VertexID` would reach the parser and be rejected. Rewrite the
                // builtins textually inside the directive so the expanded token is already the naga builtin.
                Tok::Pp(p) => {
                    let rewritten = Preprocessor(p)
                        .io_macro()
                        .unwrap_or_else(|| Preprocessor(p).vertex_builtins());
                    out.push(Tok::Pp(rewritten));
                }
                // naga's `gl_VertexIndex`/`gl_InstanceIndex` builtins are `uint`, but the GLES `gl_VertexID`/
                // `gl_InstanceID` they replace are `int`; wrap in an `int(…)` cast so a `int id = gl_VertexID;`
                // assignment keeps matching store types (naga validates this).
                Tok::Word(w) if w == "gl_VertexID" => {
                    out.push(Tok::Word("int(gl_VertexIndex)".into()))
                }
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
                // NOTE: `gl_PointSize` is deliberately NOT rewritten. WGSL has no point-size builtin, so naga's
                // `wgsl-out` genuinely cannot represent it (`Unsupported builtin PointSize`) — no textual
                // normalization can lower it faithfully. Rather than silently STRIP the assignment (which would
                // fake a green while discarding the point size the shader asked for), we let it reach naga and
                // fail, documented as the corpus's one inherent naga-24 limit (`ubo__mat2_std140`, previously the
                // documented limit, is now normalized; `builtin__gl_pointsize` is the honest remaining wall). The
                // real Chrome/GskGpu path draws instanced quads (triangles), never points, so it never emits it.
                other => out.push(other.clone()),
            }
            i += 1;
        }
        *toks = out;
    }
}

/// GskGpu declares vertex inputs and inter-stage varyings through the `IN(_loc)` / `PASS(_loc)` /
/// `PASS_FLAT(_loc)` macros, whose bodies *drop* the location (`#define IN(_loc) in`) because the GL driver
/// binds attributes by name. naga has no by-name binding: without an explicit `layout(location)` it assigns
/// every bindingless `in`/`out` location 0, and validation fails with a `BindingCollision`. The macro's
/// `_loc` argument *is* the intended location (and the same value is used for a varying in both stages, so
/// they still match), so rewrite the macro *definition* to prepend `layout(location = _loc)` — every
/// expansion then carries the explicit slot. Returns `None` for any preprocessor line that is not one of
/// these definitions.
struct Preprocessor<'a>(&'a str);

impl Preprocessor<'_> {
    fn io_macro(&self) -> Option<String> {
        let rest = self
            .0
            .trim_start()
            .strip_prefix('#')?
            .trim_start()
            .strip_prefix("define")?;
        if !rest.starts_with(char::is_whitespace) {
            return None; // `#defineX` — not a define directive
        }
        let rest = rest.trim_start();
        let paren = rest.find('(')?;
        let name = rest[..paren].trim();
        if !matches!(name, "IN" | "PASS" | "PASS_FLAT") {
            return None;
        }
        let after = &rest[paren + 1..];
        let close = after.find(')')?;
        let param = after[..close].trim();
        if param.is_empty() {
            return None;
        }
        let body = after[close + 1..].trim();
        if !matches!(body, "in" | "out" | "flat in" | "flat out") {
            return None;
        }
        Some(format!(
            "#define {name}({param}) layout(location = {param}) {body}"
        ))
    }

    fn vertex_builtins(&self) -> String {
        let source = replace_ident(self.0, "gl_VertexID", "int(gl_VertexIndex)");
        replace_ident(&source, "gl_InstanceID", "int(gl_InstanceIndex)")
    }
}

// ---------------------------------------------------------------------------------------------------
// Aggregate vertex-input / varying splitting
// ---------------------------------------------------------------------------------------------------
//
// naga requires every vertex `in`/`out` and inter-stage varying to be an *IO-shareable* type — a numeric
// scalar or vector (or a struct of such with per-member `@location`s). A matrix or an array as a *single*
// located interface member fails validation with `Argument(n, NotIOShareableType)`. GskGpu emits both:
//
//   IN(0) mat3x4 in_outline;               // a matrix vertex attribute (3 vec4 columns)
//   PASS_FLAT(2) RoundedRect _outline;     // RoundedRect == `vec4[3]`, an array varying
//
// In real desktop GL a `matCxR`/array attribute silently consumes C (or N) consecutive locations; GskGpu's
// own `_loc` numbering already leaves that room (the next input after `IN(0) mat3x4` is `IN(3)`). So we
// split each aggregate interface member into its C/N per-location vector slots (`name_hlio0…`), keep a
// private (non-interface) global of the original aggregate type so every *use* site is unchanged, and
// bridge the two at the entry point: for an input, reconstruct the global from the slots at the top of
// `main`; for an output, scatter the global into the slots at the end of `main`. No data the fragment
// stage needs is dropped — the aggregate is carried in full across the (now IO-shareable) vector slots.
//
// The generated declarations are HOISTED to just after `#version`, not left at the original declaration
// site: GskGpu's `main` (from the shared common.glsl) is emitted *before* the per-op I/O declarations, and
// the entry-point bridge we inject into `main` would otherwise reference globals GLSL has not seen yet.

/// A split aggregate interface member: `matCxR` columns or a `vec[N]` array, per-slot vector type, and the
/// spelling of a private global of the whole aggregate type.
enum AggTy {
    /// `matCxR` → `cols` columns, each a `col_ty` (`vec{rows}`); global declared with the matrix token.
    Matrix {
        tok: String,
        cols: u32,
        col_ty: String,
    },
    /// `elem[count]` array; global declared as `elem name[count]`.
    Array { elem: String, count: u32 },
}

impl AggTy {
    /// (slot count, per-slot vector type, private-global declaration for `name`).
    fn parts(&self, name: &str) -> (u32, String, String) {
        match self {
            AggTy::Matrix { tok, cols, col_ty } => {
                (*cols, col_ty.clone(), format!("{tok} {name};"))
            }
            AggTy::Array { elem, count } => {
                (*count, elem.clone(), format!("{elem} {name}[{count}];"))
            }
        }
    }
}

/// A parsed `IN(_loc) TYPE name;` / `PASS(_loc) …` / `PASS_FLAT(_loc) …` interface declaration.
struct IoDecl {
    macro_name: String, // IN | PASS | PASS_FLAT
    loc: String,        // the `_loc` argument text (numeric for a real split)
    agg: Option<AggTy>, // Some(_) only for a matrix/array member; None leaves the decl untouched
    name: String,
    end: usize, // token index just past the terminating `;`
}

struct MatrixShape {
    columns: u32,
    rows: u32,
}

impl MatrixShape {
    /// Parse `matCxR` / `matN`; reject non-matrix words (`material`, `mat`, `vec4`, …).
    fn parse(token: &str) -> Option<Self> {
        let rest = token.strip_prefix("mat")?;
        let mut dimensions = rest.split('x');
        let columns: u32 = dimensions.next()?.parse().ok()?;
        let rows = match dimensions.next() {
            Some(rows) => rows.parse().ok()?,
            None => columns,
        };
        if dimensions.next().is_some() {
            return None;
        }
        Some(Self { columns, rows })
    }
}

/// Collect object-like `#define NAME BODY` type aliases (e.g. GskGpu's `#define RoundedRect vec4[3]`) so an
/// aggregate interface member declared through the alias is recognized. Only the alias *body text* is kept.
struct TypeAliases(std::collections::HashMap<String, String>);

impl TypeAliases {
    fn from_tokens(tokens: &[Tok]) -> Self {
        let mut aliases = std::collections::HashMap::new();
        for token in tokens {
            if let Tok::Pp(preprocessor) = token {
                if let Some(rest) = preprocessor
                    .trim_start()
                    .strip_prefix('#')
                    .map(str::trim_start)
                {
                    if let Some(rest) = rest.strip_prefix("define") {
                        if rest.starts_with(char::is_whitespace) {
                            let rest = rest.trim_start();
                            let end = rest.find(|character: char| {
                                !(character == '_' || character.is_ascii_alphanumeric())
                            });
                            if let Some(end) = end {
                                let (name, body) = rest.split_at(end);
                                if body.starts_with(char::is_whitespace) && !name.is_empty() {
                                    aliases.insert(name.to_owned(), body.trim().to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
        Self(aliases)
    }

    fn classify(&self, tail: &str) -> Option<(Option<AggTy>, String)> {
        let expanded = {
            let first_end = tail
                .find(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
                .unwrap_or(tail.len());
            let (first, rest) = tail.split_at(first_end);
            match self.0.get(first) {
                Some(body) => format!("{body}{rest}"),
                None => tail.to_owned(),
            }
        };
        let tokens = Tokens::from_source(&expanded);
        let significant: Vec<&Tok> = tokens
            .iter()
            .filter(|token| token.is_significant())
            .collect();
        let base = match significant.first() {
            Some(Tok::Word(word)) => word.clone(),
            _ => return None,
        };
        let name = significant.iter().skip(1).find_map(|token| match token {
            Tok::Word(word) if word.parse::<u32>().is_err() => Some(word.clone()),
            _ => None,
        })?;
        let count = significant.iter().enumerate().find_map(|(index, token)| {
            matches!(token, Tok::Punct('['))
                .then(|| significant.get(index + 1))
                .flatten()
                .and_then(|token| match token {
                    Tok::Word(number) => number.parse().ok(),
                    _ => None,
                })
        });
        let aggregate = if let Some(matrix) = MatrixShape::parse(&base) {
            Some(AggTy::Matrix {
                tok: base.clone(),
                cols: matrix.columns,
                col_ty: format!("vec{}", matrix.rows),
            })
        } else {
            count.map(|count| AggTy::Array { elem: base, count })
        };
        Some((aggregate, name))
    }
}

/// Parse an `IN/PASS/PASS_FLAT (_loc) TYPE name [array];` declaration whose macro word is at `i`, resolving
/// a type alias for the aggregate classification. `None` if the shape does not match (left untouched).
fn parse_io_decl(toks: &[Tok], i: usize, aliases: &TypeAliases) -> Option<IoDecl> {
    let macro_name = match &toks[i] {
        Tok::Word(w) => w.clone(),
        _ => return None,
    };
    let lp = toks.next_significant(i + 1)?;
    if toks[lp] != Tok::Punct('(') {
        return None;
    }
    let rp = match_close(toks, lp, '(', ')');
    if rp >= toks.len() {
        return None;
    }
    let loc = toks[lp + 1..rp].source().trim().to_string();
    // Everything from after `)` to the terminating `;` is `TYPE name [array]`.
    let mut e = rp + 1;
    while e < toks.len() && toks[e] != Tok::Punct(';') {
        e += 1;
    }
    if e >= toks.len() {
        return None;
    }
    let tail = toks[rp + 1..e].source();
    let (agg, name) = aliases.classify(tail.trim())?;
    Some(IoDecl {
        macro_name,
        loc,
        agg,
        name,
        end: e + 1,
    })
}

/// Classify a `TYPE name [array]` interface tail (after alias expansion) into `(aggregate?, name)`. An
/// aggregate is a `matCxR` (→ per-column vectors) or an array (→ per-element slots); a scalar/vector member
/// returns `(None, name)` and is left untouched. Shared by the GskGpu macro path ([`parse_io_decl`]) and the
/// raw `layout(location=N) in/out …` ANGLE path ([`parse_raw_io_decl`]).
/// Parse a RAW ANGLE-style interface declaration `layout(location = N[, …]) [flat] (in|out) TYPE name
/// [array];` whose `layout` word is at `i`. This is the non-macro sibling of [`parse_io_decl`]: ANGLE
/// emits explicit `layout(location=N) in mat4 aModel;` / `out vec4 v[3];` (a matrix attribute or an array
/// varying) that naga rejects as a single located slot (`NotIOShareableType`), where GskGpu hid the same
/// shapes behind `IN`/`PASS` macros. Returns `None` for a scalar/vector member, a `uniform`/sampler block
/// (no `in`/`out`), a missing `location`, or a fragment `out` (a color target, never split). The direction
/// is encoded as the SAME synthetic macro name `build_io_split` already interprets, so the split/bridge
/// logic is shared verbatim.
fn parse_raw_io_decl(
    toks: &[Tok],
    i: usize,
    is_vertex: bool,
    aliases: &TypeAliases,
) -> Option<IoDecl> {
    let lp = toks.next_significant(i + 1)?;
    if toks[lp] != Tok::Punct('(') {
        return None;
    }
    let rp = match_close(toks, lp, '(', ')');
    if rp >= toks.len() {
        return None;
    }
    let loc = LayoutQualifier::parse(&toks[lp + 1..rp].source()).location()?;
    // After `)`: an optional `flat` interpolation qualifier, then the `in`/`out` storage qualifier.
    let mut j = toks.next_significant(rp + 1)?;
    let flat = matches!(&toks[j], Tok::Word(w) if w == "flat");
    if flat {
        j = toks.next_significant(j + 1)?;
    }
    let kw = match &toks[j] {
        Tok::Word(w) if w == "in" || w == "out" => w.clone(),
        _ => return None, // `uniform`, `buffer`, a type — not an interface in/out decl
    };
    // A fragment `out` is a color attachment (vec4), never an aggregate we split.
    if !is_vertex && kw == "out" {
        return None;
    }
    let mut e = j + 1;
    while e < toks.len() && toks[e] != Tok::Punct(';') {
        e += 1;
    }
    if e >= toks.len() {
        return None;
    }
    let tail = toks[j + 1..e].source();
    let (agg, name) = aliases.classify(tail.trim())?;
    // Map direction to the synthetic macro name `build_io_split` understands: a vertex input is an
    // attribute (`IN`); every other direction is a varying (`PASS`/`PASS_FLAT`), whose in/out sense
    // `build_io_split` derives from the stage.
    let macro_name = if is_vertex && kw == "in" {
        "IN"
    } else if flat {
        "PASS_FLAT"
    } else {
        "PASS"
    };
    Some(IoDecl {
        macro_name: macro_name.to_string(),
        loc,
        agg,
        name,
        end: e + 1,
    })
}

/// Rewrite dual-source-blend fragment outputs (`layout(location = L, index = X) out vec4 name;`) into a form
/// naga's `glsl-in` can parse. naga rejects the `index=` layout qualifier outright ("Unexpected qualifier")
/// and always emits `second_blend_source: false`, yet its IR and `wgsl-out` DO model dual-source blending
/// (`@second_blend_source`). So: strip the `index=` qualifier from every such `layout(...)`, and rename each
/// `index >= 1` output (declaration AND uses) with [`BLEND_SRC1_SUFFIX`]. Both sources then carry the same
/// `location = L`; the module post-pass [`crate::wgsl::fix_dual_source_blend`] flips `second_blend_source` on
/// the suffixed fragment-output member before validation, so the two same-location outputs are the legal
/// dual-source pair rather than a binding collision.
struct LayoutQualifier {
    location: Option<String>,
    index: Option<u32>,
}

impl LayoutQualifier {
    fn parse(source: &str) -> Self {
        let tokens = Tokens::from_source(source);
        let mut qualifier = Self {
            location: None,
            index: None,
        };
        for (position, token) in tokens.iter().enumerate() {
            let Tok::Word(name) = token else {
                continue;
            };
            if name != "location" && name != "index" {
                continue;
            }
            let Some(equal) = tokens.next_significant(position + 1) else {
                continue;
            };
            if tokens[equal] != Tok::Punct('=') {
                continue;
            }
            let Some(value) = tokens.next_significant(equal + 1) else {
                continue;
            };
            let Tok::Word(value) = &tokens[value] else {
                continue;
            };
            if name == "location" && value.parse::<i64>().is_ok() {
                qualifier.location = Some(value.clone());
            } else if name == "index" {
                qualifier.index = value.parse().ok();
            }
        }
        qualifier
    }

    fn location(&self) -> Option<String> {
        self.location.clone()
    }

    fn dual_source(&self) -> Option<(String, u32)> {
        self.index.map(|index| {
            (
                self.location.clone().unwrap_or_else(|| "0".to_owned()),
                index,
            )
        })
    }
}

impl Tokens {
    fn normalize_dual_source(&mut self) {
        let toks = &mut self.0;
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut second_src_names: Vec<String> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(lp) = toks.next_significant(i + 1) {
                    if toks[lp] == Tok::Punct('(') {
                        let rp = match_close(toks, lp, '(', ')');
                        if rp < toks.len() {
                            if let Some((loc, idx)) =
                                LayoutQualifier::parse(&toks[lp + 1..rp].source()).dual_source()
                            {
                                // Rewrite the whole `layout(...)` group to keep only the location.
                                out.extend(Tokens::from_source(&format!(
                                    "layout(location = {loc})"
                                )));
                                if idx >= 1 {
                                    if let Some(name) = toks.output_name(rp) {
                                        second_src_names.push(name);
                                    }
                                }
                                i = rp + 1;
                                continue;
                            }
                        }
                    }
                }
            }
            out.push(toks[i].clone());
            i += 1;
        }
        if second_src_names.is_empty() {
            *toks = out;
            return;
        }
        // Rename each index>=1 output (declaration + every use) so the module pass can find it by name.
        let mut src = out.as_slice().source();
        for n in &second_src_names {
            src = replace_ident(&src, n, &format!("{n}{BLEND_SRC1_SUFFIX}"));
        }
        *toks = Tokens::from_source(&src).0;
    }
}

/// Parse a layout-qualifier list's text for a dual-source output. Returns `Some((location, index))` only
/// when an `index` qualifier is present (`location` defaults to `"0"` if omitted); `None` otherwise.
/// The identifier name of the `out` variable declared just after a `layout(...)` group whose `)` is at
/// `rp`: `) [flat|centroid|smooth]* out TYPE NAME`. `None` if the following tokens are not an `out` decl.
/// Extract the `location = N` value from a layout-qualifier list's text (`"location = 3"`,
/// `"std140, binding = 0"`, `"location = 0, index = 1"`). `None` if no `location` qualifier is present.
/// Split every matrix/array GskGpu interface declaration into per-location vector slots + a private global,
/// and bridge the global to the slots inside `main` (reconstruct inputs on entry, scatter outputs on exit).
impl Tokens {
    fn split_aggregate_io(&mut self, stage: naga::ShaderStage) {
        let toks = &mut self.0;
        let aliases = TypeAliases::from_tokens(toks);
        let is_vertex = stage == naga::ShaderStage::Vertex;
        let mut recon: Vec<String> = Vec::new(); // input:  global[k] = slot_k;  (top of main)
        let mut scatter: Vec<String> = Vec::new(); // output: slot_k = global[k];  (end of main)
        let mut hoist: Vec<String> = Vec::new(); // split interface + private-global declarations
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if matches!(w.as_str(), "IN" | "PASS" | "PASS_FLAT"))
            {
                if let Some(decl) = parse_io_decl(toks, i, &aliases) {
                    if let Some(rep) = build_io_split(&decl, is_vertex, &mut recon, &mut scatter) {
                        // Drop the original declaration; its replacement is hoisted to the top of the file.
                        hoist.push(rep);
                        i = decl.end;
                        continue;
                    }
                }
            }
            // The raw ANGLE form: a `layout(location = N) [flat] in/out <matrix|array> name;` interface member.
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(decl) = parse_raw_io_decl(toks, i, is_vertex, &aliases) {
                    if let Some(rep) = build_io_split(&decl, is_vertex, &mut recon, &mut scatter) {
                        hoist.push(rep);
                        i = decl.end;
                        continue;
                    }
                }
            }
            out.push(toks[i].clone());
            i += 1;
        }
        if !hoist.is_empty() {
            // Insert the hoisted declarations right after the version directive (`#version` must stay first).
            let at = out
                .iter()
                .position(|t| matches!(t, Tok::Pp(p) if p.trim_start().starts_with("#version")))
                .map(|v| v + 1)
                .unwrap_or(0);
            out.insert(at, Tok::Pp(format!("\n{}\n", hoist.join("\n"))));
        }
        *toks = out;
        if !recon.is_empty() || !scatter.is_empty() {
            inject_into_main(toks, &recon, &scatter);
        }
    }
}

/// Build the replacement text for one aggregate interface declaration, appending its entry-point bridge
/// lines to `recon`/`scatter`. Returns `None` (leaving the declaration untouched) for a scalar/vector
/// member, a non-numeric location, or an `IN(…)` seen while lowering the fragment stage (dead there).
fn build_io_split(
    decl: &IoDecl,
    is_vertex: bool,
    recon: &mut Vec<String>,
    scatter: &mut Vec<String>,
) -> Option<String> {
    let agg = decl.agg.as_ref()?;
    // Direction + interpolation + whether this member is an entry-point input.
    let (dir, flat, is_input) = match decl.macro_name.as_str() {
        "IN" if is_vertex => ("in", false, true),
        "IN" => return None, // vertex attribute is dead in the fragment stage; leave it to be stripped
        "PASS" if is_vertex => ("out", false, false),
        "PASS" => ("in", false, true),
        "PASS_FLAT" if is_vertex => ("out", true, false),
        "PASS_FLAT" => ("in", true, true),
        _ => return None,
    };
    let base_loc: i64 = decl.loc.trim().parse().ok()?;
    let (count, slot_ty, global_decl) = agg.parts(&decl.name);
    let flatq = if flat { "flat " } else { "" };
    let mut s = String::new();
    for k in 0..count {
        s.push_str(&format!(
            "layout(location = {}) {flatq}{dir} {slot_ty} {}_hlio{k};\n",
            base_loc + k as i64,
            decl.name
        ));
    }
    s.push_str(&global_decl);
    for k in 0..count {
        if is_input {
            recon.push(format!("{}[{k}] = {}_hlio{k};", decl.name, decl.name));
        } else {
            scatter.push(format!("{}_hlio{k} = {}[{k}];", decl.name, decl.name));
        }
    }
    Some(s)
}

/// Insert `recon` statements just after every `main(){` and `scatter` statements just before its matching
/// `}`. Both stages' `main` bodies are visited; the one gated out by the stage `#ifdef` is stripped by
/// naga's preprocessor afterwards, so only the live `main` keeps the bridge.
fn inject_into_main(toks: &mut Vec<Tok>, recon: &[String], scatter: &[String]) {
    let pre = if recon.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", recon.join("\n"))
    };
    let post = if scatter.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", scatter.join("\n"))
    };
    let mut result: Vec<Tok> = Vec::with_capacity(toks.len());
    let mut i = 0;
    while i < toks.len() {
        if matches!(&toks[i], Tok::Word(w) if w == "main") {
            if let Some(open) = toks.main_body(i) {
                let close = match_close(toks, open, '{', '}');
                if close < toks.len() {
                    for t in &toks[i..=open] {
                        result.push(t.clone());
                    }
                    if !pre.is_empty() {
                        result.push(Tok::Pp(pre.clone()));
                    }
                    for t in &toks[open + 1..close] {
                        result.push(t.clone());
                    }
                    if !post.is_empty() {
                        result.push(Tok::Pp(post.clone()));
                    }
                    result.push(toks[close].clone());
                    i = close + 1;
                    continue;
                }
            }
        }
        result.push(toks[i].clone());
        i += 1;
    }
    *toks = result;
}

/// Index of the `{` opening the body of a `main ( … ) { … }` *definition* whose name word is at `i`; `None`
/// for anything that is not that shape (a prototype, a `main_clip_*`, …).
/// Word-boundary rewrite of the ES vertex-index builtins to naga's, wrapped in an `int(…)` cast (the naga
/// builtins are `uint`, the ES ones `int`). Used on preprocessor-line text where the builtin can hide in a
/// macro body (`#define GSK_VERTEX_INDEX gl_VertexID`) that naga's preprocessor later expands.
/// Replace every whole-identifier occurrence of `from` with `to` (neither neighbor an identifier char), so
/// `gl_VertexID` matches but `gl_VertexIDx` or a longer name containing it does not.
fn replace_ident(s: &str, from: &str, to: &str) -> String {
    let b = s.as_bytes();
    let fb = from.as_bytes();
    let is_ident = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(fb) {
            let prev_ok = i == 0 || !is_ident(b[i - 1]);
            let next_ok = i + fb.len() >= b.len() || !is_ident(b[i + fb.len()]);
            if prev_ok && next_ok {
                out.push_str(to);
                i += fb.len();
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// Rewrite every `uniform <samplerType> NAME;` global into a separate `texture`/`sampler` pair at
/// coordinated bindings (uniform block reserved at 0; sampler `k` → texture `1+2k`, sampler `2+2k`),
/// returning `(name, original sampler type)` in declaration order so [`recombine_sampler_uses`] can pick
/// the matching combining constructor for cube/array/shadow samplers.
impl Tokens {
    fn split_global_samplers(&mut self) -> Vec<(String, String)> {
        let toks = &mut self.0;
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut names: Vec<(String, String)> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "uniform") {
                // Look ahead: uniform <ws> <samplerType> <ws> NAME <ws?> [ [ .. ] ] ;
                if let Some(ty_idx) = toks.next_significant(i + 1) {
                    if let Tok::Word(ty) = toks[ty_idx].clone() {
                        if let Some(sampler) = SamplerType::parse(&ty) {
                            let (tex_ty, smp_ty) = sampler.split();
                            if let Some(name_idx) = toks.next_significant(ty_idx + 1) {
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
                                        names.push((name, ty));
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
    fn split_param_samplers(&mut self) -> Vec<(String, String)> {
        let toks = &mut self.0;
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut names: Vec<(String, String)> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if let Tok::Word(ty) = toks[i].clone() {
                if let Some(sampler) = SamplerType::parse(&ty) {
                    let (tex_ty, smp_ty) = sampler.split();
                    if let Some(name_idx) = toks.next_significant(i + 1) {
                        if let Tok::Word(name) = &toks[name_idx].clone() {
                            out.push(Tok::Word(format!(
                                "{tex_ty} {name}{TEX_SUFFIX}, {smp_ty} {name}{SMP_SUFFIX}"
                            )));
                            names.push((name.clone(), ty));
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
    fn map_es_texture_builtins(&mut self) {
        let toks = self.0.as_mut_slice();
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
                if toks
                    .next_significant(k + 1)
                    .map(|j| toks[j] == Tok::Punct('('))
                    == Some(true)
                {
                    toks[k] = Tok::Word(m.to_string());
                }
            }
        }
    }

    /// Replace each *use* of a split sampler name with either the recombined `sampler2D(NAME_hltex,
    /// NAME_hlsmp)` expression (inside a texture built-in call) or the two split arguments `NAME_hltex,
    /// NAME_hlsmp` (inside a user-function call), tracking the enclosing call while scanning.
    fn recombine_sampler_uses(&mut self, sampler_types: &[(String, String)]) {
        let toks = &mut self.0;
        // Enclosing-call stack: each `(` pushes whether its call is a texture built-in (`Some(true)`), a user
        // call (`Some(false)`), or a grouping/keyword paren (`None`).
        let mut stack: Vec<Option<bool>> = Vec::new();
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut last_word: Option<String> = None;

        for k in 0..toks.len() {
            match &toks[k] {
                Tok::Punct('(') => {
                    let ctx = match &last_word {
                        Some(word) if Identifier(word).is_keyword() => None,
                        Some(word) => Some(Identifier(word).is_texture_builtin()),
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
                Tok::Word(w) if sampler_types.iter().any(|(n, _)| n == w) => {
                    // A sampler value use. Nearest enclosing CALL determines the expansion.
                    let builtin = stack.iter().rev().find_map(|c| *c).unwrap_or(true);
                    let expansion = if builtin {
                        // Recombine with the ORIGINAL sampler type's constructor (samplerCube/2DArray/2DShadow),
                        // not a blanket `sampler2D`, so the `(texture, sampler)` halves type-check in naga.
                        let ty = sampler_types
                            .iter()
                            .find(|(n, _)| n == w)
                            .map(|(_, t)| t.as_str())
                            .unwrap_or("sampler2D");
                        let constructor = SamplerType::parse(ty)
                            .map(SamplerType::constructor)
                            .unwrap_or(ty);
                        format!("{constructor}({w}{TEX_SUFFIX}, {w}{SMP_SUFFIX})")
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
}

// ---------------------------------------------------------------------------------------------------
// std140 2-row-matrix (mat2 / matNx2) uniform-block members
// ---------------------------------------------------------------------------------------------------
//
// naga-24's `glsl-in` rejects a 2-ROW matrix (`mat2`, `mat3x2`, `mat4x2`) as a member of a `std140`
// uniform block: `front/glsl/offset.rs` errors `UnsupportedMatrixTypeInStd140`, guarded by `rows ==
// VectorSize::Bi`, because it does not model the 16-byte column padding std140 gives such a matrix. 3-/4-row
// matrices (`mat3`, `mat4`, `mat3x4`, `mat4x3`) ARE accepted. ANGLE emits `mat2` in UBOs for 2D transforms,
// so Chrome hits this wall.
//
// The fix is a std140-byte-preserving rewrite: std140 already lays out each column of a 2-row matrix in its
// own 16-byte (vec4) slot, so a `matNx2 M` member and a `vec4 M__col[N]` member occupy the IDENTICAL bytes
// (`M__col[k].xy` is column k; the upper two lanes are the padding std140 already reserves). We rewrite the
// member declaration to `vec4 M__col[N]` — the app's uploaded UBO bytes need NO re-pack — and rebuild the
// matrix value at every `block.M` USE as `matN2(block.M__col[0].xy, …, block.M__col[N-1].xy)`. A uniform is
// read-only, so `block.M` is always an rvalue; the constructor rvalue is valid in every use form it appears
// in verbatim: `block.M * v` (→ `matN2(…) * v`), `block.M[i]` / `block.M[i][j]` (indexing an rvalue matrix /
// its column vector), and passing `block.M` to a function (`f(matN2(…))`). Scoped to std140 uniform-block
// 2-row-matrix members only — a plain `uniform mat2` and a local/attribute `mat2` already validate.

/// A parsed `layout(std140, …) uniform NAME { … } [instance];` interface block: brace span, instance name
/// (only instance-named blocks are rewritten — an anonymous block's members are referenced bare), and the
/// token index just past the terminating `;`.
struct Std140Block {
    lb: usize, // index of the opening `{`
    rb: usize, // index of the matching `}`
    instance: Option<String>,
    end: usize, // index just past the terminating `;`
}

impl Std140Block {
    /// Parse a `layout(std140, …) uniform NAME { … } [instance];` block whose `layout` word is at `i`. Returns
    /// `None` for any `layout`/block that is not a `std140`-qualified uniform interface block.
    fn parse(toks: &[Tok], i: usize) -> Option<Self> {
        let lp = toks.next_significant(i + 1)?;
        if toks[lp] != Tok::Punct('(') {
            return None;
        }
        let rp = match_close(toks, lp, '(', ')');
        if rp >= toks.len() {
            return None;
        }
        let quals = toks[lp + 1..rp].source();
        // `std140` must appear as a whole qualifier word (not a substring of another identifier).
        let is_std140 = quals
            .split(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .any(|w| w == "std140");
        if !is_std140 {
            return None;
        }
        let u = toks.next_significant(rp + 1)?;
        if !matches!(&toks[u], Tok::Word(w) if w == "uniform") {
            return None;
        }
        let bn = toks.next_significant(u + 1)?; // block type name
        if !matches!(&toks[bn], Tok::Word(_)) {
            return None;
        }
        let lb = toks.next_significant(bn + 1)?;
        if toks[lb] != Tok::Punct('{') {
            return None;
        }
        let rb = match_close(toks, lb, '{', '}');
        if rb >= toks.len() {
            return None;
        }
        let after = toks.next_significant(rb + 1)?;
        let (instance, semi) = match &toks[after] {
            Tok::Word(name) => (Some(name.clone()), toks.next_significant(after + 1)?),
            Tok::Punct(';') => (None, after),
            _ => return None, // an array instance (`… x[2];`) or other shape — leave untouched
        };
        if toks[semi] != Tok::Punct(';') {
            return None;
        }
        Some(Self {
            lb,
            rb,
            instance,
            end: semi + 1,
        })
    }
}

/// Rewrite the body text of a std140 uniform block: each scalar `matNx2 NAME` member becomes `vec4
/// NAME__col[N]`, recording `(instance, NAME, cols)` in `members`. Non-2-row members are kept verbatim.
fn rewrite_std140_body(
    body: &str,
    instance: &str,
    members: &mut Vec<(String, String, u32)>,
) -> String {
    body.split(';')
        .map(|seg| match Mat2Member::parse(seg) {
            Some(member) => {
                members.push((instance.to_string(), member.name.clone(), member.columns));
                let lead: String = seg.chars().take_while(|c| c.is_whitespace()).collect();
                format!("{lead}vec4 {}__col[{}]", member.name, member.columns)
            }
            None => seg.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// If `seg` is a scalar 2-row-matrix member declaration (`mat2`/`mat3x2`/`mat4x2 NAME`, no array), return
/// `(NAME, cols)`; otherwise `None`. Precision qualifiers are already stripped before this pass runs.
struct Mat2Member {
    name: String,
    columns: u32,
}

impl Mat2Member {
    fn parse(seg: &str) -> Option<Self> {
        let toks = Tokens::from_source(seg);
        let sig: Vec<&Tok> = toks.iter().filter(|token| token.is_significant()).collect();
        if sig.iter().any(|t| matches!(t, Tok::Punct('['))) {
            return None; // an array member (`mat2 m[3]`) — not handled; leave for naga to reject
        }
        let base = match sig.first() {
            Some(Tok::Word(w)) => w.clone(),
            _ => return None,
        };
        let matrix = MatrixShape::parse(&base)?;
        if matrix.rows != 2 {
            return None;
        }
        let name = sig.iter().skip(1).find_map(|t| match t {
            Tok::Word(w) => Some(w.clone()),
            _ => None,
        })?;
        Some(Self {
            name,
            columns: matrix.columns,
        })
    }
}

/// The reconstructed matrix rvalue for a `block.member` use: `matN2(block.member__col[0].xy, …)`.
fn reconstruct_mat2(instance: &str, member: &str, cols: u32) -> String {
    let ctor = match cols {
        3 => "mat3x2",
        4 => "mat4x2",
        _ => "mat2",
    };
    let args: Vec<String> = (0..cols)
        .map(|k| format!("{instance}.{member}__col[{k}].xy"))
        .collect();
    format!("{ctor}({})", args.join(", "))
}

/// Rewrite every 2-row-matrix (`mat2`/`matNx2`) member of a `std140` uniform block to `vec4 col[N]` (same
/// std140 bytes) and reconstruct the matrix at every `block.member` use. See the module section header.
impl Tokens {
    fn split_std140_mat2(&mut self) {
        let toks = &mut self.0;
        let mut members: Vec<(String, String, u32)> = Vec::new(); // (instance, member, cols)
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(b) = Std140Block::parse(toks, i) {
                    if let Some(instance) = b.instance.clone() {
                        let before = members.len();
                        let body = toks[b.lb + 1..b.rb].source();
                        let new_body = rewrite_std140_body(&body, &instance, &mut members);
                        if members.len() == before {
                            // No 2-row-matrix member — emit the block verbatim (byte-faithful).
                            out.extend(toks[i..b.end].iter().cloned());
                        } else {
                            out.extend(toks[i..=b.lb].iter().cloned()); // through the opening `{`
                            out.extend(Tokens::from_source(&new_body));
                            out.extend(toks[b.rb..b.end].iter().cloned()); // `}` … `;`
                        }
                        i = b.end;
                        continue;
                    }
                }
            }
            out.push(toks[i].clone());
            i += 1;
        }
        *toks = out;
        if members.is_empty() {
            return;
        }
        // Replace every `instance.member` use with the reconstructed matrix constructor.
        let mut result: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if let Tok::Word(w) = &toks[i] {
                if let Some(dot) = toks.next_significant(i + 1) {
                    if toks[dot] == Tok::Punct('.') {
                        if let Some(mem) = toks.next_significant(dot + 1) {
                            if let Tok::Word(m) = &toks[mem] {
                                if let Some((_, _, cols)) = members
                                    .iter()
                                    .find(|(inst, name, _)| inst == w && name == m)
                                {
                                    result.push(Tok::Word(reconstruct_mat2(w, m, *cols)));
                                    i = mem + 1;
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
            result.push(toks[i].clone());
            i += 1;
        }
        *toks = result;
    }
}

// ---------------------------------------------------------------------------------------------------
// Comment stripping (kept local so tokenization is self-contained)
// ---------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_es_and_combined_sampler_but_not_desktop() {
        assert!(Source::new("#version 320 es\nvoid main(){}").is_es());
        assert!(Source::new("#version 300 es\nuniform sampler2D t;").is_es());
        assert!(Source::new("uniform sampler2D t; void main(){}").is_es());
        assert!(Source::new("void main(){ int i = gl_VertexID; }").is_es());
        // Already-desktop split source (what the ES2 driver path emits) must NOT be re-taken.
        let desktop = "#version 460\nlayout(binding=1) uniform texture2D t_hltex;\nlayout(binding=2) uniform sampler t_hlsmp;\nvoid main(){ vec4 c = texture(sampler2D(t_hltex,t_hlsmp), vec2(0.0)); }";
        assert!(
            !Source::new(desktop).is_es(),
            "desktop split source must keep the direct path"
        );
    }

    #[test]
    fn splits_global_sampler_and_recombines_at_builtin() {
        let src = "#version 320 es\nprecision highp float;\nuniform sampler2D uTex;\nin vec2 uv;\nout vec4 c;\nvoid main(){ c = texture(uTex, uv); }";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        assert!(out.contains("#version 460"), "{out}");
        assert!(!out.contains("320 es"), "{out}");
        assert!(!out.contains("precision"), "precision stripped: {out}");
        assert!(out.contains("uniform texture2D uTex_hltex"), "{out}");
        assert!(out.contains("uniform sampler uTex_hlsmp"), "{out}");
        assert!(
            out.contains("texture(sampler2D(uTex_hltex, uTex_hlsmp), uv)"),
            "{out}"
        );
    }

    #[test]
    fn splits_sampler_function_parameter_and_call_site() {
        let src = "#version 320 es\nuniform sampler2D uTex;\nvec4 fetch(sampler2D tex, vec2 p){ return texture(tex, p); }\nvoid main(){ gl_Position = fetch(uTex, vec2(0.0)); }";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        // parameter split into two
        assert!(
            out.contains("texture2D tex_hltex, sampler tex_hlsmp"),
            "param split: {out}"
        );
        // recombine inside helper (texture builtin)
        assert!(
            out.contains("texture(sampler2D(tex_hltex, tex_hlsmp), p)"),
            "helper recombine: {out}"
        );
        // pass split pair at the user-function call site
        assert!(
            out.contains("fetch(uTex_hltex, uTex_hlsmp, vec2(0.0))"),
            "call-site pair: {out}"
        );
    }

    // --- The REAL GskGpu constructs (verbatim shapes from the GTK4 GskGpu source our shim forwards) -------

    #[test]
    fn seeds_version_define_so_gsk_ubo_binding_branch_wins() {
        // GskGpu gates its UBO binding on `__VERSION__`, which naga's preprocessor leaves undefined (= 0),
        // so the pinned `#version 460` alone would still pick the no-binding branch. We inject the define.
        let out = Source::new("#version 320 es\n#define GSK_GLES 1\nvoid main(){}\n")
            .normalize(naga::ShaderStage::Vertex);
        assert!(out.contains("#version 460"), "version pinned: {out}");
        assert!(
            out.contains("#define __VERSION__ 460"),
            "__VERSION__ seeded: {out}"
        );
        assert!(!out.contains("320 es"), "es version gone: {out}");
    }

    #[test]
    fn rewrites_gl_vertexid_hidden_in_gsk_vertex_index_macro() {
        // The exact GskGpu form: the builtin lives only inside the macro *body*.
        let src = "#version 320 es\n#define GSK_VERTEX_INDEX gl_VertexID\nvoid main(){ int i = int(GSK_VERTEX_INDEX); }\n";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        assert!(
            out.contains("#define GSK_VERTEX_INDEX int(gl_VertexIndex)"),
            "macro body rewritten: {out}"
        );
        assert!(
            !out.contains("gl_VertexID"),
            "no raw gl_VertexID survives: {out}"
        );
    }

    #[test]
    fn adds_explicit_location_to_gsk_io_macros() {
        let src = "#version 320 es\n#define IN(_loc) in\n#define PASS(_loc) out\n#define PASS_FLAT(_loc) flat in\nvoid main(){}\n";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        assert!(
            out.contains("#define IN(_loc) layout(location = _loc) in"),
            "IN: {out}"
        );
        assert!(
            out.contains("#define PASS(_loc) layout(location = _loc) out"),
            "PASS: {out}"
        );
        assert!(
            out.contains("#define PASS_FLAT(_loc) layout(location = _loc) flat in"),
            "PASS_FLAT: {out}"
        );
    }

    #[test]
    fn rewrites_std140_mat2_member_to_vec4_columns_and_reconstructs_uses() {
        // The ANGLE mat2-in-UBO shape naga-24 rejects. The member becomes `vec4 m2__col[2]` (identical
        // std140 bytes) and each use is reconstructed with the column-vector constructor.
        let src = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat2 m2; } x;\nlayout(location = 0) in vec2 aPos;\nvoid main(){ gl_Position = vec4(x.m2 * aPos, 0.0, 1.0); }\n";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        assert!(
            out.contains("vec4 m2__col[2];"),
            "member rewritten to vec4 col array: {out}"
        );
        assert!(!out.contains("mat2 m2"), "original mat2 member gone: {out}");
        assert!(
            out.contains("mat2(x.m2__col[0].xy, x.m2__col[1].xy)"),
            "use reconstructed: {out}"
        );
        assert!(!out.contains("x.m2 "), "raw block.m2 use gone: {out}");
    }

    #[test]
    fn rewrites_std140_mat3x2_and_mat4x2_with_right_column_counts() {
        let src = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat3x2 a; mat4x2 b; } x;\nvoid main(){ vec2 p = x.a * vec3(1.0) + x.b * vec4(1.0); gl_Position = vec4(p, 0.0, 1.0); }\n";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        assert!(
            out.contains("vec4 a__col[3];"),
            "mat3x2 -> 3 columns: {out}"
        );
        assert!(
            out.contains("vec4 b__col[4];"),
            "mat4x2 -> 4 columns: {out}"
        );
        assert!(
            out.contains("mat3x2(x.a__col[0].xy, x.a__col[1].xy, x.a__col[2].xy)"),
            "mat3x2 recon: {out}"
        );
        assert!(
            out.contains("mat4x2(x.b__col[0].xy, x.b__col[1].xy, x.b__col[2].xy, x.b__col[3].xy)"),
            "mat4x2 recon: {out}"
        );
    }

    #[test]
    fn std140_mat2_pass_leaves_mat3_mat4_and_nonblock_mat2_untouched() {
        // 3-/4-row matrices in a std140 block are accepted by naga and must NOT be reshaped.
        let block = "#version 300 es\nlayout(std140, binding = 0) uniform Xf { mat3 m3; mat4 m4; } x;\nvoid main(){ gl_Position = x.m4 * vec4(x.m3 * vec3(1.0), 1.0); }\n";
        let out = Source::new(block).normalize(naga::ShaderStage::Vertex);
        assert!(
            out.contains("mat3 m3;") && out.contains("mat4 m4;"),
            "mat3/mat4 members untouched: {out}"
        );
        assert!(
            !out.contains("__col"),
            "no column rewrite for 3/4-row matrices: {out}"
        );
        // A non-block (plain global) mat2 already validates and must be left alone.
        let plain = "#version 300 es\nuniform mat2 uRot;\nlayout(location=0) in vec2 aPos;\nvoid main(){ gl_Position = vec4(uRot * aPos, 0.0, 1.0); }\n";
        let out = Source::new(plain).normalize(naga::ShaderStage::Vertex);
        assert!(
            out.contains("uniform mat2 uRot;"),
            "plain uniform mat2 untouched: {out}"
        );
        assert!(
            !out.contains("__col"),
            "no column rewrite for plain mat2: {out}"
        );
    }

    #[test]
    fn lowers_returning_switch_to_if_else_chain() {
        // A GskGpu color-state style switch: returning cases, stacked labels, and a `default`.
        let src = "#version 320 es\nint apply(uint cs){\n  switch (cs)\n    {\n    case 0u:\n      return 10;\n    case 1u:\n    case 2u:\n      return 20;\n    default:\n      return 0;\n    }\n}\nvoid main(){}\n";
        let out = Source::new(src).normalize(naga::ShaderStage::Vertex);
        assert!(!out.contains("switch"), "switch removed: {out}");
        assert!(!out.contains("case "), "case labels removed: {out}");
        assert!(out.contains("if ("), "if branch present: {out}");
        assert!(out.contains("else if ("), "else-if branch present: {out}");
        assert!(out.contains("else {"), "default became else: {out}");
        // Stacked labels 1u/2u OR into one condition.
        assert!(
            out.contains("== (1u)") && out.contains("== (2u)") && out.contains("||"),
            "stacked labels OR'd: {out}"
        );
    }
}
