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

/// A parsed `qualifier TYPE name[arr];` declaration (gl_shim.c `struct decl`). `arr` is the array element
/// count (`0` = not an array) — Skia declares default-block uniforms as arrays (`uniform vec4 uKernel[8];`)
/// which the emitted `HlUniforms` block and its std140 layout must preserve, or naga sees a scalar indexed
/// like an array and rejects the store type.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Decl {
    pub ty: String,
    pub name: String,
    pub arr: u32,
}

impl Decl {
    fn is_sampler(&self) -> bool {
        TypeToken(&self.ty).is_sampler()
    }

    fn requires_flat_interpolation(&self) -> bool {
        matches!(
            self.ty.as_str(),
            "int"
                | "uint"
                | "bool"
                | "ivec2"
                | "ivec3"
                | "ivec4"
                | "uvec2"
                | "uvec3"
                | "uvec4"
                | "bvec2"
                | "bvec3"
                | "bvec4"
        )
    }
}

struct TypeToken<'a>(&'a str);

impl TypeToken<'_> {
    fn is_sampler(&self) -> bool {
        matches!(
            self.0,
            "sampler2D"
                | "samplerCube"
                | "sampler2DArray"
                | "sampler2DShadow"
                | "samplerExternalOES"
        )
    }

    fn is_regenerated_qualifier(&self) -> bool {
        matches!(
            self.0,
            "attribute"
                | "varying"
                | "uniform"
                | "precision"
                | "const"
                | "in"
                | "out"
                | "flat"
                | "smooth"
                | "centroid"
                | "invariant"
                | "layout"
        )
    }

    fn is_precision(&self) -> bool {
        matches!(self.0, "highp" | "mediump" | "lowp")
    }

    fn is_io_qualifier(&self) -> bool {
        matches!(
            self.0,
            "flat"
                | "smooth"
                | "noperspective"
                | "centroid"
                | "sample"
                | "invariant"
                | "precise"
                | "highp"
                | "mediump"
                | "lowp"
        )
    }
}

/// A uniform-block member's byte offset/size (gl_shim.c `struct uni`).
#[derive(Clone, Debug, PartialEq)]
pub struct Uni {
    pub name: String,
    pub off: i32,
    pub sz: i32,
}

/// One GLSL stage's source text.  Scanning and source-preserving rewrites live here so callers cannot
/// accidentally mix comment-stripped offsets with the original byte stream.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    text: &'a str,
}

pub struct StageSources<'a> {
    vertex: &'a str,
    fragment: &'a str,
}

pub struct Translator;

impl<'a> StageSources<'a> {
    pub fn new(vertex: &'a str, fragment: &'a str) -> Self {
        Self { vertex, fragment }
    }
}

impl<'a> Source<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub fn vertex_attrs(self) -> Vec<Decl> {
        let text = self.comments_removed();
        let mut attrs = Tokens(&text).collect("attribute");
        attrs.truncate(16);
        append_decls_unique(&mut attrs, Tokens(&text).collect("in"), 16);
        attrs
    }

    pub fn inject_uniform_block_bindings(self) -> String {
        UniformBlockEdits::new(self.text).apply()
    }
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
impl Translator {
    pub fn compute(cs_in: &str) -> String {
        let comments = Source::new(cs_in).comments_removed();
        let mut body = Source::new(&comments).without_version();
        NormalizedSource::new(&mut body).strip_precision();
        let mut out = String::new();
        out.push_str(GLSL_VERSION);
        out.push_str(&body);
        out
    }
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
impl Source<'_> {
    fn without_version(self) -> String {
        let mut out = String::new();
        for line in self.text.lines() {
            if line.trim_start().starts_with("#version") {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Remove ES precision qualifiers from a shader body — invalid as qualifiers in desktop core GLSL.
struct NormalizedSource<'a> {
    text: &'a mut String,
}

impl<'a> NormalizedSource<'a> {
    fn new(text: &'a mut String) -> Self {
        Self { text }
    }

    fn strip_precision(&mut self) {
        wreplace(self.text, "lowp", "");
        wreplace(self.text, "mediump", "");
        wreplace(self.text, "highp", "");
    }
}

/// Emit the data-uniform interface block at `binding = 0` (matching the frame's uniform bind entry). An
/// anonymous block puts its members in global scope so the shader body references them by their plain name.
/// The sampler texture/sampler bindings start at 1 ([`emit_sampler_decls`]) so the UBO never collides.
impl Declarations<'_> {
    fn emit_uniform_block(out: &mut String, unis: &[Decl]) {
        if unis.is_empty() {
            return;
        }
        out.push_str("layout(std140, binding = 0) uniform HlUniforms {\n");
        for u in unis {
            if u.arr > 0 {
                out.push_str(&format!("    {} {}[{}];\n", u.ty, u.name, u.arr));
            } else {
                out.push_str(&format!("    {} {};\n", u.ty, u.name));
            }
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
            "sampler2DArray" => ("texture2DArray", "sampler", "sampler2DArray"),
            "sampler2DShadow" => ("texture2D", "samplerShadow", "sampler2DShadow"),
            // `samplerExternalOES` (ANGLE's YUV external image) maps to a plain 2D sampler for this bring-up —
            // correct for the single-plane RGBA path — matching the executor's `glsl_es::split_sampler_ty`.
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
            let (tex_ty, smp_ty, _) = Self::split_sampler(&s.ty);
            let tex_binding = 1 + 2 * k;
            let smp_binding = 2 + 2 * k;
            out.push_str(&format!(
                "layout(binding = {tex_binding}) uniform {tex_ty} {}_hltex;\n",
                s.name
            ));
            out.push_str(&format!(
                "layout(binding = {smp_binding}) uniform {smp_ty} {}_hlsmp;\n",
                s.name
            ));
        }
    }

    /// Rewrite each combined-sampler NAME in a shader body to a `ctor(name_hltex, name_hlsmp)` expression, so a
    /// `texture(uTex, uv)` call feeds the separated texture + sampler globals [`emit_sampler_decls`] declared.
    /// A sampler uniform can only ever appear as a texture-function argument (you cannot do arithmetic on a
    /// sampler), so a word-boundary name replace is safe and total. Run BEFORE the `texture2D(`→`texture(`
    /// lowering: `texture2D(uTex, uv)` → `texture2D(sampler2D(uTex_hltex, uTex_hlsmp), uv)` → `texture(...)`.
    fn rewrite_sampler_refs(body: &mut String, samps: &[Decl]) {
        for s in samps {
            let (_, _, ctor) = Self::split_sampler(&s.ty);
            let repl = format!("{ctor}({}_hltex, {}_hlsmp)", s.name, s.name);
            wreplace(body, &s.name, &repl);
        }
    }
}

/// Whether a GLSL-ES stage source is GskGpu/ANGLE-shaped — i.e. it uses a construct the ES2
/// reflect-and-regenerate [`translate_render`] cannot preserve, but the host executor's
/// `glsl_es`/`glsl_to_wgsl` ES route CAN: a **combined sampler type as a function parameter** (helper
/// functions like `vec4 gsk_texture(sampler2D tex, …)`, which this translator drops when it keeps only
/// `main`), or **`gl_VertexID` vertex-pulling** (no vertex attributes — this translator would reflect an
/// empty layout). For such source the driver forwards the stage VERBATIM so the executor gets the real
/// text (helpers, push-constant UBO, and all) instead of a mangled regeneration. Simple ES2 shaders
/// (`attribute`/`varying`/`gl_FragColor`, samplers only ever as globals) match NEITHER marker and keep the
/// existing [`translate_render`] path unchanged — the executor's ES route does not rewrite the ES2
/// `attribute`/`gl_FragColor` dialect, so they must stay on this path.
impl Source<'_> {
    pub fn is_forward_verbatim(self) -> bool {
        let src = self.comments_removed();
        if src.contains("gl_VertexID") || src.contains("gl_InstanceID") {
            return true;
        }
        Source::new(&src).has_sampler_parameter()
    }

    /// Detect a sampler type used as a FUNCTION PARAMETER (a sampler-type word followed by an identifier while
    /// inside a parenthesized parameter list and NOT preceded by `uniform`), the construct `translate_render`
    /// cannot carry across a helper signature.
    fn has_sampler_parameter(self) -> bool {
        let src = self.text;
        let b = src.as_bytes();
        let mut depth = 0i32;
        let mut prev_word = String::new();
        let mut i = 0usize;
        while i < b.len() {
            let c = b[i];
            if c == b'(' {
                depth += 1;
                i += 1;
                prev_word.clear();
                continue;
            }
            if c == b')' {
                depth -= 1;
                i += 1;
                prev_word.clear();
                continue;
            }
            if Tokens::is_word(c) {
                let start = i;
                while i < b.len() && Tokens::is_word(b[i]) {
                    i += 1;
                }
                let word = &src[start..i];
                // A sampler type inside a param list, not the `uniform sampler …;` global form.
                if depth > 0 && TypeToken(word).is_sampler() && prev_word != "uniform" {
                    // followed by an identifier (the parameter name), not `(` (the `sampler2D(t,s)` ctor).
                    let mut j = i;
                    while j < b.len() && Tokens::is_space(b[j]) {
                        j += 1;
                    }
                    if j < b.len() && Tokens::is_word(b[j]) {
                        return true;
                    }
                }
                prev_word = word.to_string();
                continue;
            }
            if !Tokens::is_space(c) {
                prev_word.clear();
            }
            i += 1;
        }
        false
    }
}

/// Translate a vertex+fragment GLSL-ES pair into the naga-acceptable desktop GLSL for each stage,
/// returned as `(vertex_glsl, fragment_glsl)`. Each is packed into its own `Glsl` `CreateShader` payload
/// (see [`crate::model::program::Program::link`]); the render pipeline binds them as separate modules.
impl StageSources<'_> {
    pub fn translate_render(self) -> (String, String) {
        let vs = Source::new(self.vertex).comments_removed();
        let fs = Source::new(self.fragment).comments_removed();

        let attrs = Source::new(&vs).vertex_attrs();
        let mut vary = Tokens(&vs).collect("varying");
        vary.truncate(16);
        append_decls_unique(&mut vary, Tokens(&vs).collect("out"), 16);
        let (unis, samps) = Declarations::from_stages(&vs, &fs).uniforms();
        let mut fragouts = Tokens(&fs).collect("out");
        fragouts.truncate(4);

        let mut consts = Source::new(&vs).consts();
        for c in Source::new(&fs).consts() {
            if consts.len() >= 16 {
                break;
            }
            if !consts.iter().any(|x| *x == c) {
                consts.push(c);
            }
        }

        // Carried-through globals per stage: the `struct` definitions + helper functions + plain globals the
        // reflect-and-regenerate path used to drop (leaving `main` referencing undefined types/functions). Each
        // is rewritten exactly like the `main` body (ES precision stripped, combined samplers recombined, the
        // `texture2D`/`textureCube` builtins lowered) so a helper that samples a texture stays valid.
        let (vs_structs, vs_funcs) = Source::new(&vs).partition_globals();
        let (fs_structs, fs_funcs) = Source::new(&fs).partition_globals();
        let rewrite = |items: &[String], samps: &[Decl]| -> String {
            let mut out = String::new();
            for it in items {
                let mut t = it.clone();
                NormalizedSource::new(&mut t).strip_precision();
                Declarations::rewrite_sampler_refs(&mut t, samps);
                sreplace(&mut t, "texture2D(", "texture(");
                sreplace(&mut t, "textureCube(", "texture(");
                out.push_str(&t);
                out.push('\n');
            }
            out
        };

        // ---- vertex stage ----
        let mut vs_out = String::new();
        vs_out.push_str(GLSL_VERSION);
        for c in &consts {
            vs_out.push_str(c);
            vs_out.push('\n');
        }
        vs_out.push_str(&rewrite(&vs_structs, &samps));
        for (i, a) in attrs.iter().enumerate() {
            vs_out.push_str(&format!("layout(location = {i}) in {} {};\n", a.ty, a.name));
        }
        for (j, v) in vary.iter().enumerate() {
            let flat = if v.requires_flat_interpolation() {
                "flat "
            } else {
                ""
            };
            vs_out.push_str(&format!(
                "layout(location = {j}) {flat}out {} {};\n",
                v.ty, v.name
            ));
        }
        Declarations::emit_uniform_block(&mut vs_out, &unis);
        Declarations::emit_sampler_decls(&mut vs_out, &samps);
        vs_out.push_str(&rewrite(&vs_funcs, &samps));
        let mut vb = Source::new(&vs).main_body();
        if vb.is_empty() {
            hl_log::hl_warn!(hl_log::tag::GL, "glsl vs translate: no main body");
        }
        NormalizedSource::new(&mut vb).strip_precision();
        Declarations::rewrite_sampler_refs(&mut vb, &samps);
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
        fs_out.push_str(&rewrite(&fs_structs, &samps));
        for (j, v) in vary.iter().enumerate() {
            let flat = if v.requires_flat_interpolation() {
                "flat "
            } else {
                ""
            };
            fs_out.push_str(&format!(
                "layout(location = {j}) {flat}in {} {};\n",
                v.ty, v.name
            ));
        }
        Declarations::emit_uniform_block(&mut fs_out, &unis);
        Declarations::emit_sampler_decls(&mut fs_out, &samps);
        // The fragment output(s). One output (the common case, ES2 `gl_FragColor` or a single ES3 `out`) stays
        // byte-identical: reuse the ES3 `out vec4 NAME;` if declared, else synthesize one and rewrite the ES2
        // `gl_FragColor` builtin onto it (desktop core GLSL has no `gl_FragColor`). TWO+ declared ES3 outputs
        // (MRT via glDrawBuffers) emit one `layout(location = k) out <ty> NAME;` per declared output, preserving
        // each output's type + its sequential location so the frame's N color targets receive the right value.
        let frag_name = fragouts
            .first()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "hl_FragColor".to_string());
        if fragouts.len() > 1 {
            for (k, d) in fragouts.iter().enumerate() {
                fs_out.push_str(&format!(
                    "layout(location = {k}) out {} {};\n",
                    d.ty, d.name
                ));
            }
        } else {
            fs_out.push_str(&format!("layout(location = 0) out vec4 {frag_name};\n"));
        }
        let mut fb = Source::new(&fs).main_body();
        if fb.is_empty() {
            hl_log::hl_warn!(hl_log::tag::GL, "glsl fs translate: no main body");
        }
        NormalizedSource::new(&mut fb).strip_precision();
        Declarations::rewrite_sampler_refs(&mut fb, &samps);
        sreplace(&mut fb, "texture2D(", "texture(");
        sreplace(&mut fb, "textureCube(", "texture(");
        if fragouts.is_empty() {
            wreplace(&mut fb, "gl_FragColor", &frag_name);
        }
        fs_out.push_str(&rewrite(&fs_funcs, &samps));
        fs_out.push_str(&format!("void main() {{\n{fb}\n}}\n"));

        (vs_out, fs_out)
    }
}

/// Rewrite texture-coordinate arguments for selected samplers from GL's bottom-left render-target
/// convention to the host GPU's top-left convention. Uploaded texture planes retain their existing
/// orientation; only samplers backed by a rendered FBO are named by the caller.
impl Source<'_> {
    pub(crate) fn flip_render_target_samplers(self, samplers: &[String]) -> String {
        let source = self.text;
        if samplers.is_empty() {
            return source.to_string();
        }
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0;
        while let Some(relative) = source[cursor..].find("texture(") {
            let call = cursor + relative;
            let open = call + "texture".len();
            out.push_str(&source[cursor..open + 1]);

            let mut depth = 1usize;
            let mut commas = Vec::new();
            let mut close = None;
            for (relative, ch) in source[open + 1..].char_indices() {
                let at = open + 1 + relative;
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(at);
                            break;
                        }
                    }
                    ',' if depth == 1 => commas.push(at),
                    _ => {}
                }
            }
            let Some(close) = close else {
                out.push_str(&source[open + 1..]);
                return out;
            };
            let Some(&first_comma) = commas.first() else {
                out.push_str(&source[open + 1..=close]);
                cursor = close + 1;
                continue;
            };
            let sampler = source[open + 1..first_comma].trim();
            if samplers
                .iter()
                .any(|name| sampler == name || sampler.contains(name))
            {
                let coord_end = commas.get(1).copied().unwrap_or(close);
                let coord = source[first_comma + 1..coord_end].trim();
                out.push_str(&source[open + 1..first_comma + 1]);
                out.push_str(" vec2((");
                out.push_str(coord);
                out.push_str(").x, 1.0 - (");
                out.push_str(coord);
                out.push_str(").y)");
                out.push_str(&source[coord_end..=close]);
            } else {
                out.push_str(&source[open + 1..=close]);
            }
            cursor = close + 1;
        }
        out.push_str(&source[cursor..]);
        out
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL parsing / reflection helpers (shared by the desktop-GLSL emit above and the query/introspection
// reflection). Ported from hl-shim-gl/src/translate.rs.
// ---------------------------------------------------------------------------------------------------

impl Tokens<'_> {
    #[inline]
    fn is_space(c: u8) -> bool {
        matches!(c, b' ' | b'\t' | b'\n' | b'\r')
    }
    #[inline]
    fn is_word(c: u8) -> bool {
        c == b'_' || c.is_ascii_alphanumeric()
    }
    fn is_precision_or_interp(t: &str) -> bool {
        matches!(
            t,
            "lowp" | "mediump" | "highp" | "flat" | "smooth" | "centroid"
        )
    }

    /// Brace/paren nesting depth of byte offset `at` within `b` (counting only the delimiters before it). A
    /// TOP-LEVEL interface declaration sits at `(0, 0)`; anything inside a function body (`{ … }`) or a
    /// parameter list (`( … )`) is nested. Used by [`collect`] to reject an `in`/`out`/`inout` FUNCTION
    /// PARAMETER (paren depth > 0) or a body-local declaration (brace depth > 0), which are not interface decls.
    fn depth_at(b: &[u8], at: usize) -> (i32, i32) {
        let (mut brace, mut paren) = (0i32, 0i32);
        for &c in &b[..at.min(b.len())] {
            match c {
                b'{' => brace += 1,
                b'}' => brace -= 1,
                b'(' => paren += 1,
                b')' => paren -= 1,
                _ => {}
            }
        }
        (brace, paren)
    }

    /// `collect` — parse TOP-LEVEL `kw TYPE name;` declarations from `src` (skipping precision/interpolation
    /// qualifiers before the type). Only declarations at brace/paren depth `(0, 0)` are interface declarations;
    /// a keyword occurrence inside a function body or a parameter list (e.g. an `out`/`inout` parameter, or an
    /// `in` vertex-puller helper argument) is NOT one and is skipped — otherwise a helper's `out float a`
    /// parameter would be reflected as a phantom fragment output / varying / attribute.
    fn collect(&self, kw: &str) -> Vec<Decl> {
        let src = self.0;
        let b = src.as_bytes();
        let kb = kw.as_bytes();
        let kl = kb.len();
        let mut out = Vec::new();
        let mut p = 0usize;
        while let Some(rel) = find_from(b, kb, p) {
            let at = rel;
            let before_word = at != 0 && Self::is_word(b[at - 1]);
            let after_word = at + kl < b.len() && Self::is_word(b[at + kl]);
            if before_word || after_word {
                p = at + kl;
                continue;
            }
            if Self::depth_at(b, at) != (0, 0) {
                p = at + kl;
                continue;
            }
            let mut q = at + kl;
            while q < b.len() && Self::is_space(b[q]) {
                q += 1;
            }
            let read_tok = |q: &mut usize| -> String {
                let mut s = String::new();
                while *q < b.len() && !Self::is_space(b[*q]) && b[*q] != b';' && s.len() < 15 {
                    s.push(b[*q] as char);
                    *q += 1;
                }
                s
            };
            let mut ty = read_tok(&mut q);
            while Self::is_precision_or_interp(&ty) {
                while q < b.len() && Self::is_space(b[q]) {
                    q += 1;
                }
                ty = read_tok(&mut q);
            }
            while q < b.len() && Self::is_space(b[q]) {
                q += 1;
            }
            // std140 interface block: `uniform Block { TYPE m; ... } [inst];` — enumerate the MEMBERS.
            if q < b.len() && b[q] == b'{' {
                q += 1;
                while out.len() < 32 {
                    while q < b.len() && Self::is_space(b[q]) {
                        q += 1;
                    }
                    if q >= b.len() || b[q] == b'}' {
                        break;
                    }
                    let mut mty = read_tok(&mut q);
                    while Self::is_precision_or_interp(&mty) {
                        while q < b.len() && Self::is_space(b[q]) {
                            q += 1;
                        }
                        mty = read_tok(&mut q);
                    }
                    while q < b.len() && Self::is_space(b[q]) {
                        q += 1;
                    }
                    let mut mnm = String::new();
                    while q < b.len() && Self::is_word(b[q]) && mnm.len() < 31 {
                        mnm.push(b[q] as char);
                        q += 1;
                    }
                    let marr = Self::read_array_subscript(b, &mut q);
                    while q < b.len() && b[q] != b';' && b[q] != b'}' {
                        q += 1; // skip to the member end
                    }
                    if q < b.len() && b[q] == b';' {
                        q += 1;
                    }
                    if !mty.is_empty() && !mnm.is_empty() {
                        out.push(Decl {
                            ty: mty,
                            name: mnm,
                            arr: marr,
                        });
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
            while q < b.len() && Self::is_word(b[q]) && nm.len() < 31 {
                nm.push(b[q] as char);
                q += 1;
            }
            let arr = Self::read_array_subscript(b, &mut q);
            if !ty.is_empty() && !nm.is_empty() {
                out.push(Decl { ty, name: nm, arr });
            }
            p = q;
        }
        out
    }

    /// If the bytes at `*q` are an array subscript `[N]` (possibly with surrounding spaces), consume it and
    /// return `N` (the element count); otherwise leave `*q` unchanged and return `0`. Only a plain integer size
    /// is captured — a non-literal size (`[SOME_MACRO]`) yields `0` (treated as a scalar, best-effort).
    fn read_array_subscript(b: &[u8], q: &mut usize) -> u32 {
        let mut p = *q;
        while p < b.len() && Self::is_space(b[p]) {
            p += 1;
        }
        if p >= b.len() || b[p] != b'[' {
            return 0;
        }
        p += 1;
        while p < b.len() && Self::is_space(b[p]) {
            p += 1;
        }
        let s = p;
        while p < b.len() && b[p].is_ascii_digit() {
            p += 1;
        }
        let digits = &b[s..p];
        while p < b.len() && Self::is_space(b[p]) {
            p += 1;
        }
        if p < b.len() && b[p] == b']' && !digits.is_empty() {
            if let Ok(n) = std::str::from_utf8(digits).unwrap_or("").parse::<u32>() {
                *q = p + 1;
                return n;
            }
        }
        0
    }
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// Extract the body between `void main(){` and its DEPTH-MATCHED closing `}`. `main` is matched at a word
/// boundary and must be followed by `(` — so a carried helper whose name merely CONTAINS "main" (e.g.
/// `mainImage`) never hijacks the scan — and the closing brace is found by brace-depth counting rather than
/// the last `}` in the source, so a helper function emitted AFTER `main` does not swallow the body.
impl Source<'_> {
    fn main_body(self) -> String {
        let b = self.text.as_bytes();
        let n = b.len();
        let mut i = 0usize;
        let mut open = None;
        while let Some(rel) = find_from(b, b"main", i) {
            let before = rel == 0 || !Tokens::is_word(b[rel - 1]);
            let after = rel + 4 >= n || !Tokens::is_word(b[rel + 4]);
            if before && after {
                let mut j = rel + 4;
                while j < n && Tokens::is_space(b[j]) {
                    j += 1;
                }
                if j < n && b[j] == b'(' {
                    if let Some(brace_rel) = b[j..].iter().position(|&c| c == b'{') {
                        open = Some(j + brace_rel);
                        break;
                    }
                }
            }
            i = rel + 4;
        }
        let p = match open {
            Some(p) => p,
            None => return String::new(),
        };
        let mut depth = 0i32;
        let mut k = p;
        while k < n {
            match b[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return String::from_utf8_lossy(&b[p + 1..k]).into_owned();
                    }
                }
                _ => {}
            }
            k += 1;
        }
        String::new()
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
            if !Tokens::is_word(before) && !Tokens::is_word(after) {
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
impl Source<'_> {
    fn consts(self) -> Vec<String> {
        let b = self.text.as_bytes();
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
            if Tokens::is_word(before) || (after != b' ' && after != b'\t') {
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
}

/// Strip `//` and `/* */` comments (gl_shim.c `strip_comments`).
impl Source<'_> {
    fn comments_removed(self) -> String {
        let b = self.text.as_bytes();
        let n = b.len();
        let mut out = String::with_capacity(n);
        let mut r = 0;
        let mut quote = None;
        while r < n {
            if let Some(delimiter) = quote {
                out.push(b[r] as char);
                if b[r] == b'\\' && r + 1 < n {
                    r += 1;
                    out.push(b[r] as char);
                } else if b[r] == delimiter {
                    quote = None;
                }
                r += 1;
            } else if b[r] == b'\'' || b[r] == b'"' {
                quote = Some(b[r]);
                out.push(b[r] as char);
                r += 1;
            } else if r + 1 < n && b[r] == b'/' && b[r + 1] == b'/' {
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
}

/// Strip every preprocessor line (`#version`/`#define`/`#ifdef`/…). `translate_render` pins its own desktop
/// `#version` and reflects the interface directly, so ES preprocessor directives are dropped on this path —
/// the macro-heavy GskGpu/ANGLE shaders that actually depend on them take the VERBATIM host route (whose
/// `glsl_es::normalize` runs naga's real preprocessor) instead of the reflect-and-regenerate path.
impl Source<'_> {
    fn without_preprocessor(self) -> String {
        let mut out = String::new();
        for line in self.text.lines() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Split a (comment- and preprocessor-stripped) stage into TOP-LEVEL units — one per `struct`/interface
/// declaration, global variable, or function definition — by tracking brace depth: a `;` at depth 0 ends a
/// simple declaration; a `}` returning to depth 0 ends a block (a `struct`/uniform-block declaration then
/// absorbs an optional instance name + terminating `;`, while a function definition ends at the `}`). This
/// is what lets [`partition_globals`] carry the struct definitions and helper functions the old
/// reflect-and-regenerate path silently dropped (leaving `main` referencing undefined types/functions).
impl Source<'_> {
    fn top_level_units(self) -> Vec<String> {
        let src = self.text;
        let b = self.text.as_bytes();
        let n = b.len();
        let mut units = Vec::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        let mut i = 0usize;
        while i < n {
            match b[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth <= 0 {
                        depth = 0;
                        // A block just closed. If it is a `struct`/uniform block, an optional instance name and a
                        // terminating `;` belong to THIS unit; a function definition (no trailing `;`) ends here.
                        let mut j = i + 1;
                        while j < n && Tokens::is_space(b[j]) {
                            j += 1;
                        }
                        if j < n && b[j] == b';' {
                            j += 1; // `struct S { … };`
                        } else if j < n && Tokens::is_word(b[j]) {
                            let mut k = j;
                            while k < n && Tokens::is_word(b[k]) {
                                k += 1;
                            }
                            let mut m = k;
                            while m < n && Tokens::is_space(b[m]) {
                                m += 1;
                            }
                            if m < n && b[m] == b';' {
                                j = m + 1; // `uniform B { … } inst;`
                            } else {
                                j = i + 1; // a following declaration — this unit is a function, ends at `}`
                            }
                        }
                        let u = src[start..j].trim();
                        if !u.is_empty() {
                            units.push(u.to_string());
                        }
                        start = j;
                        i = j;
                        continue;
                    }
                }
                b';' if depth == 0 => {
                    let u = src[start..=i].trim();
                    if !u.is_empty() {
                        units.push(u.to_string());
                    }
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        let tail = src[start..].trim();
        if !tail.is_empty() {
            units.push(tail.to_string());
        }
        units
    }
}

/// The first whitespace-delimited word of a top-level unit (its leading qualifier/type/keyword).
struct Tokens<'a>(&'a str);

impl<'a> Tokens<'a> {
    fn first_word(&self) -> &'a str {
        let u = self.0;
        u.split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
    }
}

/// A leading qualifier/keyword whose top-level unit is an INTERFACE declaration the translator REGENERATES
/// (attributes/varyings/uniforms) or a `precision`/`const` statement handled elsewhere — so it is NOT carried
/// verbatim. Everything else at top level (a `struct`, a helper function, a plain global) IS carried.
/// Partition a stage's carried-through globals into `(struct_defs, functions_and_globals)`, in source order.
/// The interface declarations the translator regenerates (attribute/varying/uniform/const/precision) are
/// dropped here; struct definitions are separated so they can be emitted BEFORE the uniform block and helper
/// functions that reference them. `main` is excluded (its body is emitted by [`main_body`]). This is the fix
/// for ANGLE/real-world shaders that declare helper functions, `struct`s, array-of-struct locals, etc. — the
/// old path kept only `main`, so any such shader failed to compile with "unknown function/type".
impl Source<'_> {
    fn partition_globals(self) -> (Vec<String>, Vec<String>) {
        let stripped = self.without_preprocessor();
        let mut structs = Vec::new();
        let mut funcs = Vec::new();
        for u in Source::new(&stripped).top_level_units() {
            let fw = Tokens(&u).first_word();
            if TypeToken(fw).is_regenerated_qualifier() {
                continue;
            }
            if fw == "struct" {
                structs.push(u);
                continue;
            }
            // A function definition/prototype whose name is `main` is emitted by `main_body`, not carried.
            if let Some(paren) = u.find('(') {
                let before_brace = u.find('{').map_or(true, |bp| paren < bp);
                if before_brace {
                    let name = u[..paren].trim_end();
                    let name = name
                        .rsplit(|c: char| c.is_whitespace() || c == '*')
                        .next()
                        .unwrap_or("");
                    if name == "main" {
                        continue;
                    }
                }
            }
            funcs.push(u);
        }
        (structs, funcs)
    }
}

/// Whether a varying/interface type is an integer/bool aggregate that desktop GLSL (and naga) REQUIRE to
/// carry the `flat` interpolation qualifier — an `int`/`uint`/`bool` (or `ivecN`/`uvecN`/`bvecN`) varying
/// cannot be smoothly interpolated. GLSL-ES declares these `flat`, but the reflection [`collect`] drops the
/// qualifier, so the regenerated declaration must re-add it or the stage fails to compile.
/// Collect uniforms from vs+fs (dedup by name), split into DATA uniforms and SAMPLER uniforms
/// (gl_shim.c `collect_uniforms`). Returns `(data, samplers)` capped at 16 / 4.
struct Declarations<'a> {
    vertex: &'a str,
    fragment: &'a str,
}

impl<'a> Declarations<'a> {
    fn from_stages(vertex: &'a str, fragment: &'a str) -> Self {
        Self { vertex, fragment }
    }

    fn uniforms(self) -> (Vec<Decl>, Vec<Decl>) {
        let mut all = Tokens(self.vertex).collect("uniform");
        all.truncate(32);
        for d in Tokens(self.fragment).collect("uniform") {
            if all.len() >= 32 {
                break;
            }
            if !all.iter().any(|x| x.name == d.name) {
                all.push(d);
            }
        }
        let (mut data, mut samps) = (Vec::new(), Vec::new());
        for d in all {
            if d.is_sampler() {
                if samps.len() < 4 {
                    samps.push(d);
                }
            } else if data.len() < 16 {
                data.push(d);
            }
        }
        (data, samps)
    }
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
/// MSL struct member (size, align) for a GLSL uniform type (gl_shim.c `msl_type_layout`).
impl TypeToken<'_> {
    fn layout(&self) -> Option<(i32, i32)> {
        Some(match self.0 {
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
}

/// Compute the uniform-block byte layout (name→offset/size) matching Metal's struct alignment
/// (gl_shim.c `uni_layout`). Returns `(members, total_bytes)`.
impl StageSources<'_> {
    pub fn uniform_layout(self) -> (Vec<Uni>, i32) {
        let vs = Source::new(self.vertex).comments_removed();
        let fs = Source::new(self.fragment).comments_removed();
        let (unis, _samps) = Declarations::from_stages(&vs, &fs).uniforms();
        let mut cur = 0i32;
        let mut out = Vec::new();
        for d in unis.iter().take(16) {
            let (esz, eal) = TypeToken(&d.ty).layout().unwrap_or((4, 4));
            // std140: an ARRAY member rounds each element's stride UP to a vec4 (16 B) and aligns the member to
            // 16 B; a scalar/vector/matrix member keeps its natural size/alignment.
            let (sz, al) = if d.arr > 0 {
                let stride = (esz + 15) & !15;
                (stride * d.arr as i32, eal.max(16))
            } else {
                (esz, eal)
            };
            cur = (cur + al - 1) & !(al - 1);
            out.push(Uni {
                name: d.name.clone(),
                off: cur,
                sz,
            });
            cur += sz;
        }
        let total = (cur + 15) & !15;
        (out, total)
    }
}

/// One declared uniform BLOCK: its `layout(binding = N)` point + its ordered member declarations. Used by
/// [`crate::service::record`] to route a MULTI-block program (two `glBindBufferRange`d ranges bound to
/// distinct binding points, each feeding its own block) — the translator flattens every block's members
/// into one `HlUniforms` block at IR binding 0, so the recorded bytes must be assembled block-by-block from
/// each block's own bound range in declaration order.
#[derive(Clone, Debug, PartialEq)]
pub struct UniformBlockDecl {
    pub binding: u32,
    pub members: Vec<Decl>,
}

/// Enumerate every uniform BLOCK (`layout(binding=N) uniform Name { members }`) a program declares, across
/// both stages, in the SAME declaration order [`collect_uniforms`] flattens them into `HlUniforms` (vertex
/// stage first, then fragment-only blocks), deduped by binding point. Plain `uniform TYPE name;` data /
/// sampler uniforms are NOT blocks and are skipped. Returns an empty vec for a program with no interface
/// block (the default-uniform `glUniform*` path).
impl StageSources<'_> {
    pub fn uniform_blocks(self) -> Vec<UniformBlockDecl> {
        let mut out: Vec<UniformBlockDecl> = Vec::new();
        for src in [self.vertex, self.fragment] {
            let src = Source::new(src).comments_removed();
            for blk in Source::new(&src).uniform_blocks() {
                if !out.iter().any(|b| b.binding == blk.binding) {
                    out.push(blk);
                }
            }
        }
        out
    }
}

/// Scan ONE (comment-stripped) stage for its `uniform Name { … }` blocks, capturing each block's
/// `binding = N` (from the preceding `layout(...)`, default `0`) and its ordered member decls.
impl Source<'_> {
    fn uniform_blocks(self) -> Vec<UniformBlockDecl> {
        let src = self.text;
        let b = src.as_bytes();
        let mut out = Vec::new();
        let mut p = 0usize;
        while let Some(rel) = find_from(b, b"uniform", p) {
            let before = rel != 0 && Tokens::is_word(b[rel - 1]);
            let after = rel + 7 < b.len() && Tokens::is_word(b[rel + 7]);
            if before || after {
                p = rel + 7;
                continue;
            }
            // Skip the block NAME token, then require `{` (a plain `uniform TYPE name;` is not a block).
            let mut q = rel + 7;
            while q < b.len() && Tokens::is_space(b[q]) {
                q += 1;
            }
            while q < b.len() && Tokens::is_word(b[q]) {
                q += 1;
            }
            while q < b.len() && Tokens::is_space(b[q]) {
                q += 1;
            }
            if q >= b.len() || b[q] != b'{' {
                p = rel + 7;
                continue;
            }
            // The block's binding from the immediately-preceding `layout(...)` (default 0).
            let binding = src[..rel]
                .rfind("layout")
                .map(|lpos| &src[lpos..rel])
                .and_then(|seg| seg.find("binding").map(|bp| &seg[bp + "binding".len()..]))
                .map(|tail| {
                    tail.chars()
                        .skip_while(|c| !c.is_ascii_digit())
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                })
                .and_then(|d| d.parse::<u32>().ok())
                .unwrap_or(0);
            // Parse members `TYPE name;` until `}` (skipping precision/interpolation qualifiers before TYPE).
            q += 1; // past `{`
            let mut members = Vec::new();
            while q < b.len() && b[q] != b'}' && members.len() < 32 {
                while q < b.len() && (Tokens::is_space(b[q]) || b[q] == b';') {
                    q += 1;
                }
                if q >= b.len() || b[q] == b'}' {
                    break;
                }
                let read_tok = |q: &mut usize| -> String {
                    let mut s = String::new();
                    while *q < b.len()
                        && !Tokens::is_space(b[*q])
                        && b[*q] != b';'
                        && b[*q] != b'}'
                        && s.len() < 31
                    {
                        s.push(b[*q] as char);
                        *q += 1;
                    }
                    s
                };
                let mut ty = read_tok(&mut q);
                while Tokens::is_precision_or_interp(&ty) {
                    while q < b.len() && Tokens::is_space(b[q]) {
                        q += 1;
                    }
                    ty = read_tok(&mut q);
                }
                while q < b.len() && Tokens::is_space(b[q]) {
                    q += 1;
                }
                let name = read_tok(&mut q);
                let arr = Tokens::read_array_subscript(b, &mut q);
                if !ty.is_empty() && !name.is_empty() {
                    members.push(Decl { ty, name, arr });
                }
            }
            out.push(UniformBlockDecl { binding, members });
            p = q.max(rel + 7);
        }
        out
    }
}

/// The explicit binding point a data-uniform BLOCK declares in its `layout(...)` qualifier — the GL
/// binding index the app's `glBindBufferBase(GL_UNIFORM_BUFFER, N, buffer)` targets (GskGpu/GTK4 declares
/// `layout(std140, binding = 0) uniform PushConstants { … }`). Scans `src` for a uniform-BLOCK declaration
/// (`uniform NAME {`), then reads `binding = N` from the immediately-preceding `layout(...)`. Returns
/// `Some(N)` (or `Some(0)` for a block with no explicit `binding`), or `None` if `src` declares no uniform
/// block (only plain `uniform TYPE name;` data/sampler uniforms — those never carry a block binding). Used
/// to resolve which `glBindBufferBase`d buffer feeds the shader's std140 UBO at IR binding 0.
impl Source<'_> {
    pub fn uniform_block_binding(self) -> Option<u32> {
        let src = Source::new(self.text).comments_removed();
        let b = src.as_bytes();
        let mut p = 0usize;
        while let Some(rel) = find_from(b, b"uniform", p) {
            let before = rel != 0 && Tokens::is_word(b[rel - 1]);
            let after = rel + 7 < b.len() && Tokens::is_word(b[rel + 7]);
            if before || after {
                p = rel + 7;
                continue;
            }
            // A BLOCK is `uniform NAME {` — skip the name, then require `{` (a plain `uniform TYPE name;`
            // data/sampler uniform has no `{` and is not a block).
            let mut q = rel + 7;
            while q < b.len() && Tokens::is_space(b[q]) {
                q += 1;
            }
            while q < b.len() && Tokens::is_word(b[q]) {
                q += 1;
            }
            while q < b.len() && Tokens::is_space(b[q]) {
                q += 1;
            }
            if q < b.len() && b[q] == b'{' {
                // The block's `layout(...)` qualifier sits just before the `uniform` keyword; read `binding = N`.
                if let Some(lpos) = src[..rel].rfind("layout") {
                    let seg = &src[lpos..rel];
                    if let Some(bpos) = seg.find("binding") {
                        let digits: String = seg[bpos + "binding".len()..]
                            .chars()
                            .skip_while(|c| !c.is_ascii_digit())
                            .take_while(|c| c.is_ascii_digit())
                            .collect();
                        if let Ok(n) = digits.parse::<u32>() {
                            return Some(n);
                        }
                    }
                }
                return Some(0);
            }
            p = rel + 7;
        }
        None
    }
}

/// Inject `layout(binding = N)` into every uniform BLOCK that LACKS an explicit binding, before a stage is
/// forwarded VERBATIM to the host's naga `glsl-in`. GLSL-ES 3.00 allows a bindingless `uniform Block { … }`,
/// but naga's `glsl-in` REQUIRES the binding (`uniform/buffer blocks require layout(binding=X)`), so
/// Chrome/ANGLE's forward-verbatim GLSL — which declares its blocks WITHOUT a binding — otherwise fails
/// `CreateRenderPipeline`. GskGpu/GTK4 already writes `layout(std140, binding = 0) uniform PushConstants`,
/// so its blocks ALREADY carry a binding and are left byte-for-byte untouched; a stage whose every block is
/// already bound is returned unchanged (the whole GskGpu verbatim path stays identical).
///
/// The injected N is the block's ORDINAL among the stage's uniform blocks. For the dominant single-block
/// Chrome shape that is `binding = 0`, matching the frame builder's binding-0 UBO
/// ([`crate::service::frame::build_frame_ir`]) and the `glBindBufferBase(GL_UNIFORM_BUFFER, 0, …)`
/// resolution in [`crate::service::record`] (both key off the ORIGINAL `vs_src`/`fs_src`, whose bindingless
/// block reflects as binding `0` too — so the injected IR and the byte resolution agree). An existing
/// `layout(std140)`/`layout(std430)` qualifier is PRESERVED — `binding = N` is merged into its list; a block
/// with no `layout(...)` at all gets a fresh `layout(binding = N)` prepended. Combined sampler globals are
/// deliberately NOT touched: the host executor's `glsl_es::split_global_samplers` splits each
/// `uniform sampler2D s;` into a `texture`/`sampler` pair and assigns their `1+2k`/`2+2k` bindings itself,
/// so injecting here would double-qualify them.
struct UniformBlockEdits<'a> {
    src: &'a str,
}

impl<'a> UniformBlockEdits<'a> {
    fn new(src: &'a str) -> Self {
        Self { src }
    }

    fn apply(self) -> String {
        let src = self.src;
        let b = src.as_bytes();
        let n = b.len();
        // (byte position, text to insert there) — collected in ascending position order.
        let mut edits: Vec<(usize, String)> = Vec::new();
        let mut ordinal: u32 = 0;
        let mut i = 0usize;
        while i < n {
            // Skip comments so a `uniform` inside one is never matched (the forwarded source keeps comments —
            // naga runs its own preprocessor/comment strip on the result).
            if i + 1 < n && b[i] == b'/' && b[i + 1] == b'/' {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                continue;
            }
            // Match the `uniform` keyword at a word boundary.
            let is_kw = b[i..].starts_with(b"uniform")
                && (i == 0 || !Tokens::is_word(b[i - 1]))
                && (i + 7 >= n || !Tokens::is_word(b[i + 7]));
            if !is_kw {
                i += 1;
                continue;
            }
            // A BLOCK is `uniform NAME {` — skip the block name, then require `{` (a plain `uniform TYPE name;`
            // data/sampler uniform has no `{` and is left alone).
            let mut q = i + 7;
            while q < n && Tokens::is_space(b[q]) {
                q += 1;
            }
            let name_start = q;
            while q < n && Tokens::is_word(b[q]) {
                q += 1;
            }
            let has_name = q > name_start;
            while q < n && Tokens::is_space(b[q]) {
                q += 1;
            }
            if !(has_name && q < n && b[q] == b'{') {
                i += 7; // a `uniform TYPE name;` — not a block; skip past the keyword.
                continue;
            }
            // A uniform block. Resolve its immediately-preceding `layout(...)` qualifier(s), if any.
            let (merge_at, has_binding) = Self::preceding_layout_binding(b, i);
            if !has_binding {
                match merge_at {
                    // Merge into the existing `layout(...)` list: insert `, binding = N` before its `)`.
                    Some(rparen) => edits.push((rparen, format!(", binding = {ordinal}"))),
                    // No layout at all: prepend a fresh qualifier before the `uniform` keyword.
                    None => edits.push((i, format!("layout(binding = {ordinal}) "))),
                }
            }
            ordinal += 1;
            i = q + 1;
        }
        if edits.is_empty() {
            return src.to_string();
        }
        let mut out = String::with_capacity(n + edits.len() * 20);
        let mut last = 0usize;
        for (pos, text) in &edits {
            out.push_str(&src[last..*pos]);
            out.push_str(text);
            last = *pos;
        }
        out.push_str(&src[last..]);
        out
    }

    /// For the uniform-BLOCK whose `uniform` keyword is at `uniform_pos`, walk backward over the block's
    /// immediately-preceding `layout(...)` qualifier group(s) (whitespace-separated). Returns
    /// `(merge_position, has_binding)`: `merge_position` is `Some(byte index of the rightmost group's `)`)` (the
    /// point to splice `, binding = N` into) or `None` when the block has no preceding `layout(...)`; `has_binding`
    /// is true when ANY of those groups already declares a `binding`, in which case the block is left untouched.
    fn preceding_layout_binding(b: &[u8], uniform_pos: usize) -> (Option<usize>, bool) {
        let mut end = uniform_pos;
        while end > 0 && Tokens::is_space(b[end - 1]) {
            end -= 1;
        }
        if end == 0 || b[end - 1] != b')' {
            return (None, false);
        }
        let rparen = end - 1;
        // Find the `(` matching this `)`.
        let mut depth = 0i32;
        let mut k = rparen;
        let lparen = loop {
            match b[k] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        break k;
                    }
                }
                _ => {}
            }
            if k == 0 {
                return (None, false);
            }
            k -= 1;
        };
        // The `layout` keyword must sit just before `(` (whitespace allowed) for this to be a layout group.
        let mut p = lparen;
        while p > 0 && Tokens::is_space(b[p - 1]) {
            p -= 1;
        }
        if p < 6 || &b[p - 6..p] != b"layout" || (p > 6 && Tokens::is_word(b[p - 7])) {
            return (None, false);
        }
        let group_has_binding = b[lparen..=rparen].windows(7).any(|w| w == b"binding");
        // Chained groups (`layout(std140) layout(binding=0) uniform …`): recurse to the earlier group, but keep
        // the RIGHTMOST group's `)` as the merge point (closest to the block).
        let (_, earlier_binding) = Self::preceding_layout_binding(b, p - 6);
        (Some(rparen), group_has_binding || earlier_binding)
    }
}

/// Prepare ONE stage of a forward-VERBATIM program for naga's `glsl-in`: wrap its bare default-block data
/// uniforms into the binding-0 `HlUniforms` block ([`wrap_default_block_uniforms`]) AND inject a binding
/// into any explicit uniform block that lacks one ([`inject_uniform_block_bindings`]). `combined` is the
/// program's data uniforms across BOTH stages (from [`program_uniform_decls`]) so the wrapped block's std140
/// layout matches the `Program::ubuf` bytes the frame builder binds at binding 0. A stage with no bare data
/// uniforms and only already-bound blocks (GskGpu/GTK4) is returned byte-identical.
impl Source<'_> {
    pub fn prepare_verbatim_stage(self, combined: &[Decl]) -> String {
        let wrapped = self.wrap_default_block_uniforms(combined);
        Source::new(&wrapped).inject_uniform_block_bindings()
    }
}

/// Wrap a verbatim stage's BARE default-block data uniforms (`uniform highp vec4 x;` at global scope) into a
/// single anonymous `layout(std140, binding = 0) uniform HlUniforms { … };` block naga's `glsl-in` accepts —
/// GLSL-ES 3.00's implicit default uniform block is rejected by naga ("uniform/buffer blocks require
/// layout(binding=X)"). Skia (Chrome's GPU-raster) emits `uniform highp vec4 sk_RTAdjust;` style default
/// uniforms; GskGpu/GTK4 keeps its uniforms inside an explicit `layout(std140, binding=0) uniform
/// PushConstants` block, so it has NO bare depth-0 data uniform and is left byte-identical.
///
/// `combined` is the program's data uniforms from BOTH stages ([`program_uniform_decls`], the same list
/// [`uni_layout`] lays out into `Program::ubuf`), so the emitted block carries the FULL combined member set
/// in the FULL std140 layout the frame builder binds at binding 0 — a stage that references only a subset
/// keeps its members at the shared offsets (an unused member is harmless). The bare declarations are removed
/// and the block spliced in at the first removed site; a sampler global or a block member is never touched.
/// Returns the source unchanged when the stage has no bare depth-0 data uniform.
impl Source<'_> {
    fn wrap_default_block_uniforms(self, combined: &[Decl]) -> String {
        let src = self.text;
        if combined.is_empty() {
            return src.to_string();
        }
        let b = src.as_bytes();
        let n = b.len();
        // Byte spans of the bare `uniform … name;` declarations to delete (depth-0, non-block, non-sampler).
        let mut removals: Vec<(usize, usize)> = Vec::new();
        let mut brace: i32 = 0;
        let mut i = 0usize;
        while i < n {
            // Skip comments so a `uniform` inside one is never matched.
            if i + 1 < n && b[i] == b'/' && b[i + 1] == b'/' {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                continue;
            }
            match b[i] {
                b'{' => {
                    brace += 1;
                    i += 1;
                    continue;
                }
                b'}' => {
                    brace -= 1;
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let is_kw = brace == 0
                && b[i..].starts_with(b"uniform")
                && (i == 0 || !Tokens::is_word(b[i - 1]))
                && (i + 7 >= n || !Tokens::is_word(b[i + 7]));
            if !is_kw {
                i += 1;
                continue;
            }
            // Read the type token (skipping precision/interpolation qualifiers), mirroring `collect`.
            let mut q = i + 7;
            let read_tok = |q: &mut usize| -> String {
                while *q < n && Tokens::is_space(b[*q]) {
                    *q += 1;
                }
                let start = *q;
                while *q < n && (Tokens::is_word(b[*q])) {
                    *q += 1;
                }
                String::from_utf8_lossy(&b[start..*q]).into_owned()
            };
            let mut ty = read_tok(&mut q);
            while Tokens::is_precision_or_interp(&ty) {
                ty = read_tok(&mut q);
            }
            while q < n && Tokens::is_space(b[q]) {
                q += 1;
            }
            // A BLOCK (`uniform NAME { … }`) or a sampler global is NOT a bare data uniform — leave it.
            if q < n && b[q] == b'{' {
                i += 7;
                continue;
            }
            if TypeToken(&ty).is_sampler() {
                i += 7;
                continue;
            }
            // `uniform TYPE name … ;` — a bare default-block data uniform; delete the whole statement.
            let mut e = q;
            while e < n && b[e] != b';' {
                e += 1;
            }
            if e < n {
                e += 1; // include the ';'
                removals.push((i, e));
                i = e;
                continue;
            }
            i += 7;
        }
        if removals.is_empty() {
            return src.to_string();
        }
        // Emit the combined std140 block once, at the first removed declaration's site.
        let mut block = String::new();
        Declarations::emit_uniform_block(&mut block, combined);
        let insert_at = removals[0].0;
        let mut out = String::with_capacity(n + block.len());
        let mut cursor = 0usize;
        for (idx, (s, e)) in removals.iter().enumerate() {
            out.push_str(&src[cursor..*s]);
            if idx == 0 {
                // Splice the block where the first bare declaration was.
                debug_assert_eq!(*s, insert_at);
                out.push_str(&block);
            }
            cursor = *e;
        }
        out.push_str(&src[cursor..]);
        out
    }
}

/// Prepare BOTH stages of a forward-VERBATIM program for naga's `glsl-in`: wrap each stage's bare
/// default-block uniforms into the binding-0 `HlUniforms` block + inject uniform-block bindings
/// ([`prepare_verbatim_stage`]), THEN inject `layout(location = N)` into bare `in`/`out` attribute/varying
/// declarations across the two stages ([`inject_io_locations`]). Location injection is a PROGRAM-level step
/// (not per-stage) because a vertex-shader `out NAME` and the fragment-shader `in NAME` for the SAME varying
/// must receive the SAME location for `CreateRenderPipeline`'s inter-stage match — so it needs both stages
/// together. A GskGpu/GTK4 program (uniforms in a bound block, varyings via `IN()`/`PASS()` macros that
/// already carry `layout(location=)`) has no bare data uniform and no bare depth-0 `in`/`out`, so both steps
/// return their stage byte-identical.
pub fn prepare_verbatim_program(vs: &str, fs: &str, combined: &[Decl]) -> (String, String) {
    let vs_u = Source::new(vs).prepare_verbatim_stage(combined);
    let fs_u = Source::new(fs).prepare_verbatim_stage(combined);
    StageSources::new(&vs_u, &fs_u).inject_io_locations()
}

/// A depth-0 `in`/`out` interface declaration found in a verbatim stage (an attribute, a varying, or a
/// fragment output) — the unit [`inject_io_locations`] assigns a `layout(location = N)` to.
struct IoDecl {
    /// `true` for `in` (vertex attribute / fragment varying-in), `false` for `out` (vertex varying-out /
    /// fragment render-target).
    is_in: bool,
    /// The declared identifier — the key that name-matches a vertex `out` to the fragment `in` varying.
    name: String,
    /// Byte offset of the statement's first qualifier token (where a fresh `layout(location = N) ` is
    /// prepended when the decl carries no `layout(...)` group).
    stmt_start: usize,
    /// Byte offset of the closest preceding `layout(...)` group's `)`, when one exists without a `location`
    /// (the point to splice `, location = N` into). `None` when the decl has no `layout(...)` at all.
    merge_rparen: Option<usize>,
    /// `Some(_)` when the decl ALREADY declares `layout(location = …)` (parsed value, or `u32::MAX` when the
    /// value is a macro/non-integer) — such a decl is PRESERVED untouched and its location reserved.
    explicit_loc: Option<u32>,
}

/// A precision qualifier that may sit between the `in`/`out` storage keyword and the type (`in highp vec4`).
/// A qualifier that may PRECEDE the `in`/`out` storage keyword in an interface declaration (interpolation /
/// auxiliary / invariant / precision) — walked over backward to find the statement start.
/// Scan a verbatim stage for its depth-0 `in`/`out` interface declarations (Skia's BARE
/// `in highp vec4 fillBounds; flat out mediump vec4 vcolor_S0;` and any already-`layout(location=)` ones).
/// Function-parameter `in`/`out` (paren depth > 0), body-local declarations (brace depth > 0), `#`-directive
/// lines (the GskGpu `#define IN(_loc) layout(location = _loc) in` macro bodies), comments, and interface
/// BLOCKS (`out NAME { … }`) are all skipped — so only real global attribute/varying/output decls are found.
impl Declarations<'_> {
    fn scan_io_decls(src: &str) -> Vec<IoDecl> {
        let b = src.as_bytes();
        let n = b.len();
        let mut out = Vec::new();
        let (mut brace, mut paren) = (0i32, 0i32);
        let mut i = 0usize;
        let mut line_start = true; // true while only whitespace has been seen since the last newline
        while i < n {
            let c = b[i];
            // A preprocessor directive: skip the whole logical line (keeps GskGpu's `#define … in`/`out` macro
            // definitions out of the scan). Line continuations (`\` before newline) extend the skip.
            if line_start && c == b'#' {
                while i < n && b[i] != b'\n' {
                    if b[i] == b'\\' && i + 1 < n {
                        i += 1;
                    }
                    i += 1;
                }
                continue;
            }
            if c == b'\n' {
                line_start = true;
                i += 1;
                continue;
            }
            if Tokens::is_space(c) {
                i += 1;
                continue; // leading whitespace keeps line_start true
            }
            if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                line_start = false;
                continue;
            }
            if matches!(c, b'{' | b'}' | b'(' | b')') {
                match c {
                    b'{' => brace += 1,
                    b'}' => brace -= 1,
                    b'(' => paren += 1,
                    b')' => paren -= 1,
                    _ => {}
                }
                line_start = false;
                i += 1;
                continue;
            }
            if Tokens::is_word(c) {
                let start = i;
                while i < n && Tokens::is_word(b[i]) {
                    i += 1;
                }
                line_start = false;
                let word = &src[start..i];
                if brace == 0 && paren == 0 && (word == "in" || word == "out") {
                    if let Some(name) = Self::parse_io_decl_forward(b, i) {
                        let (stmt_start, merge_rparen, explicit_loc) =
                            Self::preceding_io_qualifiers(b, start);
                        out.push(IoDecl {
                            is_in: word == "in",
                            name,
                            stmt_start,
                            merge_rparen,
                            explicit_loc,
                        });
                    }
                }
                continue;
            }
            line_start = false;
            i += 1;
        }
        out
    }

    /// Forward from just-after an `in`/`out` keyword (`q`): confirm a clean interface declaration
    /// `[precision] TYPE NAME [ [array] ] ;` and return the declared NAME. Returns `None` for an interface
    /// BLOCK (`… NAME {`), an initializer (`… = …`), or any shape that is not a plain varying/attribute/output
    /// (so a stray `in`/`out` is never rewritten).
    fn parse_io_decl_forward(b: &[u8], q: usize) -> Option<String> {
        let n = b.len();
        let mut p = q;
        let read_word = |p: &mut usize| -> String {
            while *p < n && Tokens::is_space(b[*p]) {
                *p += 1;
            }
            let s = *p;
            while *p < n && Tokens::is_word(b[*p]) {
                *p += 1;
            }
            String::from_utf8_lossy(&b[s..*p]).into_owned()
        };
        // The type token, skipping any precision qualifier (`in highp vec4 …`).
        let mut ty = read_word(&mut p);
        while TypeToken(&ty).is_precision() {
            ty = read_word(&mut p);
        }
        if ty.is_empty() {
            return None;
        }
        // The declared name.
        let name = read_word(&mut p);
        if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
            return None;
        }
        while p < n && Tokens::is_space(b[p]) {
            p += 1;
        }
        // An interface BLOCK (`out NAME { … }`) is not a plain decl — leave it.
        if p < n && b[p] == b'{' {
            return None;
        }
        // An optional array suffix `[ … ]`.
        if p < n && b[p] == b'[' {
            while p < n && b[p] != b']' {
                p += 1;
            }
            if p < n {
                p += 1; // consume ']'
            }
            while p < n && Tokens::is_space(b[p]) {
                p += 1;
            }
        }
        // Must terminate at `;` (no initializer, no comma-list — Skia declares one varying per statement).
        if p < n && b[p] == b';' {
            Some(name)
        } else {
            None
        }
    }

    /// Walk backward from an `in`/`out` keyword over its preceding qualifier tokens (interpolation / precision /
    /// invariant) and `layout(...)` group(s). Returns `(stmt_start, merge_rparen, explicit_loc)`: `stmt_start` is
    /// the first qualifier byte (prepend point for a fresh `layout`), `merge_rparen` the closest preceding
    /// `layout(...)`'s `)` (splice point for `, location = N`) or `None`, and `explicit_loc` the already-declared
    /// `location` value (`u32::MAX` if present but non-integer) or `None`.
    fn preceding_io_qualifiers(b: &[u8], kw_start: usize) -> (usize, Option<usize>, Option<u32>) {
        let mut p = kw_start;
        let mut merge_rparen: Option<usize> = None;
        let mut explicit: Option<u32> = None;
        loop {
            // Look back over whitespace for a candidate qualifier ENDING at `q`. `p` stays anchored at the last
            // CONFIRMED qualifier/keyword start, so a non-qualifier (or start-of-file) leaves `stmt_start = p`
            // and does not swallow the leading newline/indent before the keyword.
            let mut q = p;
            while q > 0 && Tokens::is_space(b[q - 1]) {
                q -= 1;
            }
            if q == 0 {
                break;
            }
            let ch = b[q - 1];
            if ch == b')' {
                let rparen = q - 1;
                // Match the `(`.
                let mut depth = 0i32;
                let mut k = rparen;
                let lparen = loop {
                    match b[k] {
                        b')' => depth += 1,
                        b'(' => {
                            depth -= 1;
                            if depth == 0 {
                                break k;
                            }
                        }
                        _ => {}
                    }
                    if k == 0 {
                        return (p, merge_rparen, explicit);
                    }
                    k -= 1;
                };
                // `layout` must precede the `(` for this to be a layout group.
                let mut pp = lparen;
                while pp > 0 && Tokens::is_space(b[pp - 1]) {
                    pp -= 1;
                }
                if pp >= 6 && &b[pp - 6..pp] == b"layout" && !(pp > 6 && Tokens::is_word(b[pp - 7]))
                {
                    let grp = &b[lparen..=rparen];
                    if grp.windows(8).any(|w| w == b"location") {
                        explicit = explicit
                            .or_else(|| Some(Self::parse_location_value(grp).unwrap_or(u32::MAX)));
                    }
                    if merge_rparen.is_none() {
                        merge_rparen = Some(rparen);
                    }
                    p = pp - 6; // continue before the `layout` keyword
                    continue;
                }
                break;
            }
            if Tokens::is_word(ch) {
                let mut ws = q - 1;
                while ws > 0 && Tokens::is_word(b[ws - 1]) {
                    ws -= 1;
                }
                let w = String::from_utf8_lossy(&b[ws..q]);
                if TypeToken(&w).is_io_qualifier() {
                    p = ws;
                    continue;
                }
                break;
            }
            break;
        }
        (p, merge_rparen, explicit)
    }

    /// Parse the integer after `location` in a `layout(...)` group's bytes (`layout(location = 3)` → `3`).
    /// Returns `None` when the value is not a plain integer (a macro parameter such as `location = _loc`).
    fn parse_location_value(grp: &[u8]) -> Option<u32> {
        let pos = grp.windows(8).position(|w| w == b"location")?;
        let mut p = pos + 8;
        while p < grp.len() && (Tokens::is_space(grp[p]) || grp[p] == b'=') {
            p += 1;
        }
        let s = p;
        while p < grp.len() && grp[p].is_ascii_digit() {
            p += 1;
        }
        if p > s {
            std::str::from_utf8(&grp[s..p]).ok()?.parse().ok()
        } else {
            None
        }
    }
}

/// Inject `layout(location = N)` into the BARE (bindingless) depth-0 `in`/`out` declarations of a verbatim
/// vertex+fragment program so naga's validator does not collapse every one to location 0
/// (`BindingCollision { location: 0 }`). Skia declares its attributes/varyings/outputs BARE
/// (`in highp vec4 fillBounds; flat out mediump vec4 vcolor_S0;`) — it binds attributes by NAME
/// (`glBindAttribLocation`), which naga has no notion of — so we assign locations here.
///
/// Three SEPARATE location namespaces (matching naga's per-entry-point argument/return binding spaces, and
/// the ES2 [`translate_render`] scheme):
///   * VERTEX `in` attributes — sequential in declaration order.
///   * VARYINGS — a vertex `out` and the fragment `in` of the SAME NAME share ONE location (the inter-stage
///     contract `CreateRenderPipeline` checks). Assigned in vertex-`out` declaration order, then any
///     fragment-only `in` continues the counter.
///   * FRAGMENT `out` render targets — sequential in declaration order.
/// A decl that already carries `layout(location = …)` (ANGLE's explicit form, or GskGpu's `IN()`/`PASS()`
/// macro expansion) is PRESERVED and its location reserved. Interpolation (`flat`) and precision (`highp`)
/// qualifiers are preserved (we only prepend/merge a `layout`). A program with no bare depth-0 `in`/`out`
/// (GskGpu/GTK4) is returned byte-identical.
impl StageSources<'_> {
    pub fn inject_io_locations(self) -> (String, String) {
        let vs = self.vertex;
        let fs = self.fragment;
        use std::collections::{BTreeMap, BTreeSet};
        let vsd = Declarations::scan_io_decls(vs);
        let fsd = Declarations::scan_io_decls(fs);

        // Lowest free location not yet used in `used`, then reserve it.
        let take = |used: &mut BTreeSet<u32>| -> u32 {
            let mut c = 0u32;
            while used.contains(&c) {
                c += 1;
            }
            used.insert(c);
            c
        };

        // Reserve explicitly-numbered locations so a bare decl never collides with them.
        let mut attr_used: BTreeSet<u32> = vsd
            .iter()
            .filter(|d| d.is_in)
            .filter_map(|d| d.explicit_loc.filter(|&l| l != u32::MAX))
            .collect();
        let mut fs_out_used: BTreeSet<u32> = fsd
            .iter()
            .filter(|d| !d.is_in)
            .filter_map(|d| d.explicit_loc.filter(|&l| l != u32::MAX))
            .collect();

        // Shared varying map (name → location) seeded from explicit vertex-`out` / fragment-`in` decls.
        let mut varying_map: BTreeMap<String, u32> = BTreeMap::new();
        let mut varying_used: BTreeSet<u32> = BTreeSet::new();
        for d in vsd.iter().filter(|d| !d.is_in) {
            if let Some(l) = d.explicit_loc.filter(|&l| l != u32::MAX) {
                varying_map.entry(d.name.clone()).or_insert(l);
                varying_used.insert(l);
            }
        }
        for d in fsd.iter().filter(|d| d.is_in) {
            if let Some(l) = d.explicit_loc.filter(|&l| l != u32::MAX) {
                varying_map.entry(d.name.clone()).or_insert(l);
                varying_used.insert(l);
            }
        }
        // Assign bare varyings: vertex `out` in declaration order, then any fragment-only `in`.
        for d in vsd.iter().filter(|d| !d.is_in && d.explicit_loc.is_none()) {
            varying_map
                .entry(d.name.clone())
                .or_insert_with(|| take(&mut varying_used));
        }
        for d in fsd.iter().filter(|d| d.is_in && d.explicit_loc.is_none()) {
            varying_map
                .entry(d.name.clone())
                .or_insert_with(|| take(&mut varying_used));
        }

        // Emit per-stage edits (position, inserted text) in declaration order.
        let edit_for = |d: &IoDecl, loc: u32| -> (usize, String) {
            match d.merge_rparen {
                Some(rp) => (rp, format!(", location = {loc}")),
                None => (d.stmt_start, format!("layout(location = {loc}) ")),
            }
        };
        let mut vs_edits: Vec<(usize, String)> = Vec::new();
        for d in &vsd {
            if d.explicit_loc.is_some() {
                continue;
            }
            let loc = if d.is_in {
                take(&mut attr_used)
            } else {
                varying_map[&d.name]
            };
            vs_edits.push(edit_for(d, loc));
        }
        let mut fs_edits: Vec<(usize, String)> = Vec::new();
        for d in &fsd {
            if d.explicit_loc.is_some() {
                continue;
            }
            let loc = if d.is_in {
                varying_map[&d.name]
            } else {
                take(&mut fs_out_used)
            };
            fs_edits.push(edit_for(d, loc));
        }
        (
            Edits::from(vs_edits).apply(vs),
            Edits::from(fs_edits).apply(fs),
        )
    }
}

/// Apply ascending-position insertion edits to `src` (each `(pos, text)` splices `text` in BEFORE byte
/// `pos`). Sorted by position so a merge-into-`layout` edit and a prepend edit stay ordered.
#[hl_design::naming(
    reason = "edits is the collection noun for ordered GLSL source transformations"
)]
struct Edits(Vec<(usize, String)>);

impl From<Vec<(usize, String)>> for Edits {
    fn from(edits: Vec<(usize, String)>) -> Self {
        Self(edits)
    }
}

impl Edits {
    fn apply(mut self, src: &str) -> String {
        if self.0.is_empty() {
            return src.to_string();
        }
        self.0.sort_by_key(|(position, _)| *position);
        let mut out = String::with_capacity(src.len() + self.0.len() * 24);
        let mut last = 0usize;
        for (pos, text) in &self.0 {
            out.push_str(&src[last..*pos]);
            out.push_str(text);
            last = *pos;
        }
        out.push_str(&src[last..]);
        out
    }
}

/// The samplers a linked program declares, in declaration order (for `glUniform1i` → texture-unit
/// mapping and the bind-group emission).
impl StageSources<'_> {
    pub fn samplers(self) -> Vec<String> {
        let vs = Source::new(self.vertex).comments_removed();
        let fs = Source::new(self.fragment).comments_removed();
        let (_data, samps) = Declarations::from_stages(&vs, &fs).uniforms();
        samps.into_iter().map(|d| d.name).collect()
    }
}

/// The sampler types the host executor's ES route (`hl-gpu-wgpu::glsl_es::split_sampler_ty`) splits +
/// numbers, mapped to whether the type is the external-image form. This MUST match that host set exactly:
/// the host counts EVERY one of these in a `layout(binding=)` running index, so the driver's binding
/// numbering has to count the same set to agree (see [`verbatim_sampler_bindings`]). The driver's own
/// reflection ([`is_sampler_type`]) is a NARROWER set (no `samplerExternalOES`/`sampler2DArray`) because it
/// only classifies the sampler NAMES it binds; binding NUMBERS need the wider host set.
impl TypeToken<'_> {
    fn host_sampler(&self) -> Option<bool> {
        match self.0 {
            "sampler2D" | "samplerCube" | "sampler2DArray" | "sampler2DShadow" => Some(false),
            "samplerExternalOES" => Some(true),
            _ => None,
        }
    }
}

/// The host bind-group binding INDEX `k` (→ texture `1+2k`, sampler `2+2k`) each of `samp_names` lands on
/// when its FRAGMENT source is forwarded VERBATIM to the host executor's ES route. The host
/// (`glsl_es::split_global_samplers`) assigns `k` by a running counter over EVERY host-recognized
/// `uniform <samplerType> NAME;` in fragment TEXT order — crucially counting the `samplerExternalOES`
/// declarations in the inactive `#ifdef …_IS_EXTERNAL` branches too (GskGpu declares each texture as an
/// external OR a `sampler2D` triple). The driver's own `program_samplers` reflection skips those external
/// decls and dedups, so its declaration index does NOT equal the host's `k` (e.g. the active
/// `sampler2D GSK_TEXTURE0` is host-`k=1`, not `0`, because the external `GSK_TEXTURE0` before it consumed
/// `k=0`). This replays the host's counter to recover the true `k` per bound sampler name.
///
/// The external branch preprocesses OUT (external images are unsupported in this bring-up), so a name's
/// COMPILED declaration is its non-external one; we therefore prefer the non-external `k` when a name has
/// both (keeping an external-only name's `k` as a fallback). For plain (non-GskGpu) verbatim source with no
/// `#if` sampler branches this yields the identity `k == declaration index`, so simple shaders are unchanged.
impl StageSources<'_> {
    pub fn verbatim_sampler_bindings(self, samp_names: &[String]) -> Vec<u32> {
        // A dedicated word scan (NOT `collect`, whose 15-char type cap would truncate `samplerExternalOES` and
        // miss it — the host's tokenizer has no such cap and DOES count it). Match `uniform <samplerType> NAME`
        // exactly as `glsl_es::split_global_samplers` does: `uniform`, then a host-recognized sampler type, then
        // the sampler name; `k` increments once per such declaration in text order.
        let src = Source::new(self.fragment).comments_removed();
        let b = src.as_bytes();
        let mut words: Vec<(usize, &str)> = Vec::new();
        let mut i = 0usize;
        while i < b.len() {
            if Tokens::is_word(b[i]) {
                let start = i;
                while i < b.len() && Tokens::is_word(b[i]) {
                    i += 1;
                }
                words.push((start, &src[start..i]));
            } else {
                i += 1;
            }
        }
        let mut map: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
        let mut k = 0u32;
        let mut w = 0usize;
        while w + 2 < words.len() {
            if words[w].1 == "uniform" {
                if let Some(is_external) = TypeToken(words[w + 1].1).host_sampler() {
                    let name = words[w + 2].1;
                    // Prefer the non-external (compiled) declaration's k; keep external only if it's the sole
                    // decl for this name (external image branches preprocess OUT in this bring-up).
                    if !is_external || !map.contains_key(name) {
                        map.insert(name, k);
                    }
                    k += 1; // the host counts every recognized sampler decl, active branch or not.
                    w += 3;
                    continue;
                }
            }
            w += 1;
        }
        samp_names
            .iter()
            .enumerate()
            .map(|(i, n)| map.get(n.as_str()).copied().unwrap_or(i as u32))
            .collect()
    }
}

/// The DATA uniforms a linked program declares, in declaration order, as `(name, glsl_type)` — the
/// reflection `glGetActiveUniform` reports. Matches the order of the uniform-block layout ([`uni_layout`])
/// and the location convention of [`Program::uniform_location`](crate::model::program::Program::uniform_location)
/// (data uniforms first, then samplers), so the two never disagree.
impl StageSources<'_> {
    pub fn uniform_decls(self) -> Vec<Decl> {
        let vs = Source::new(self.vertex).comments_removed();
        let fs = Source::new(self.fragment).comments_removed();
        let (data, _samps) = Declarations::from_stages(&vs, &fs).uniforms();
        data
    }

    /// The SAMPLER uniforms as `(name, glsl_type)` (`program_samplers` keeps only names; this keeps the
    /// declared sampler type so `glGetActiveUniform` can report `GL_SAMPLER_2D` vs `GL_SAMPLER_CUBE`).
    pub fn sampler_decls(self) -> Vec<Decl> {
        let vs = Source::new(self.vertex).comments_removed();
        let fs = Source::new(self.fragment).comments_removed();
        let (_data, samps) = Declarations::from_stages(&vs, &fs).uniforms();
        samps
    }

    /// The fragment-shader output variables a linked program declares (`out vecN name;`), in declaration
    /// order — the resource list `glGetFragDataLocation`/`glGetProgramResource*(GL_PROGRAM_OUTPUT)` resolve
    /// against. An ES2-style `gl_FragColor` shader declares none (its single output is location 0 implicitly).
    pub fn frag_outputs(self) -> Vec<Decl> {
        let fs = Source::new(self.fragment).comments_removed();
        let mut outs = Tokens(&fs).collect("out");
        outs.truncate(4);
        outs
    }
}
