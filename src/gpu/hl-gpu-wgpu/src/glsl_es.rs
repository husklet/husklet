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

    /// Rewrite every 2-row-matrix member of a `std140` uniform block to its `vec4 col[N]` column form
    /// (identical std140 bytes) and reconstruct the matrix at each use, because naga's `glsl-in` rejects
    /// `matNx2` in std140 outright (`UnsupportedMatrixTypeInStd140`, `front/glsl/offset.rs`).
    ///
    /// DIALECT-INDEPENDENT, so it is applied on BOTH routes rather than only inside [`Self::normalize`].
    /// naga's restriction has nothing to do with GLSL-ES: the GL driver's ES2 path rewrites its shaders to
    /// desktop form before they arrive, which makes [`Self::is_es`] false and skips `normalize` entirely —
    /// so a plain `uniform mat2` collected into the driver's default-uniform block was never reached by
    /// this pass at all. A shader with no 2-row matrix in a std140 block is returned unchanged, so the
    /// unconditional application is byte-faithful (the same contract as [`Self::rewrite_isinf`]).
    pub(crate) fn split_std140_mat2(&self) -> String {
        if !self.text.contains("std140") {
            return self.text.to_string();
        }
        let mut toks = Tokens::from_source(self.text);
        toks.split_std140_mat2();
        toks.0.as_slice().source()
    }

    /// Drop the `index = N` layout qualifier and mark each `index >= 1` fragment output, so
    /// `crate::wgsl::fix_dual_source_blend` can turn it into a `@second_blend_source`.
    ///
    /// DIALECT-INDEPENDENT: naga's `glsl-in` cannot PARSE `index =` in any dialect, so a desktop-form
    /// shader using `EXT_blend_func_extended`-style dual-source blending was refused outright while the
    /// identical ES shader compiled. Byte-faithful when no `index =` qualifier is present, and idempotent
    /// — the rewrite leaves only `location`, so a second application finds nothing.
    pub(crate) fn normalize_dual_source(&self) -> String {
        let mut toks = Tokens::from_source(self.text);
        toks.normalize_dual_source();
        toks.0.as_slice().source()
    }

    /// Lower `switch` to an `if`/`else` chain.
    ///
    /// DIALECT-INDEPENDENT: `switch` is valid in both GLSL-ES 3.0 and desktop GLSL, and what cannot accept
    /// it is the TARGET — naga's `wgsl-out` refuses a fall-through case block. A desktop-form shader whose
    /// switch returns from its cases was refused while the identical ES shader compiled. Idempotent: the
    /// lowering leaves no `switch` behind.
    pub(crate) fn lower_switch(&self) -> String {
        if !self.text.contains("switch") {
            return self.text.to_string();
        }
        let toks = Tokens::from_source(self.text);
        SwitchRewrite::lower_all(&toks).as_slice().source()
    }

    /// Split every MATRIX (and aggregate) interface member into per-location vector slots plus a private
    /// global, bridging the two inside `main` — the only form WGSL can express, since a matrix cannot be a
    /// shader input or output there at all.
    ///
    /// DIALECT-INDEPENDENT, and applied on BOTH routes for the same reason as
    /// [`Self::split_std140_mat2`]: WGSL's restriction on interface types has nothing to do with GLSL-ES,
    /// but this pass lived only inside [`Self::normalize`], which the GL driver's output skips because it
    /// rewrites its shaders to desktop form before they arrive. A plain `layout(location = N) out mat3 v;`
    /// was therefore split on the ES route and refused on the desktop one — the same gate that hid the
    /// two-row-matrix rewrite, in a second pass.
    ///
    /// Idempotent: a unit with nothing left to split is returned unchanged, so applying it after
    /// `normalize` has already run costs nothing.
    pub(crate) fn split_aggregate_io(&self, stage: naga::ShaderStage) -> String {
        let mut toks = Tokens::from_source(self.text);
        toks.split_aggregate_io(stage);
        toks.0.as_slice().source()
    }

    /// Redirect the fixed-unity point-size builtin on both ES and driver-produced desktop routes.
    pub(crate) fn normalize_fixed_point_size(&self, stage: naga::ShaderStage) -> String {
        let mut toks = Tokens::from_source(self.text);
        toks.normalize_fixed_point_size(stage);
        toks.0.as_slice().source()
    }

    /// Rewrite every narrow-element array member of a `std140` uniform block (`float u[4]`, `vec2 u[2]`,
    /// `int u[16]`, …) to the equivalent array of 4-component vectors (`vec4 u__arr[4]`), swizzling the
    /// original value back at each use. The uniform address space requires a 16-byte array stride in both
    /// WGSL and std140, but naga's `glsl-in` carries the element type's NATURAL stride into the module, so
    /// the emitted WGSL is refused by wgpu's validator ("array stride 4 is not a multiple of the required
    /// alignment 16"). The driver already writes these elements 16 bytes apart, so the rewrite describes
    /// the bytes that are actually uploaded.
    ///
    /// DIALECT-INDEPENDENT for the same reason as [`Self::split_std140_mat2`] — the layout rule has nothing
    /// to do with GLSL-ES — so it is applied on BOTH routes. A shader with no such member is returned
    /// unchanged.
    pub(crate) fn pad_std140_arrays(&self) -> String {
        if !self.text.contains("std140") {
            return self.text.to_string();
        }
        let mut toks = Tokens::from_source(self.text);
        toks.pad_std140_arrays();
        toks.0.as_slice().source()
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
        toks.normalize_fixed_point_size(stage);
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
