use super::*;

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
pub(super) const GLSL_VERSION: &str = "#version 460\n";

/// Strip a leading `#version …` directive line (ES or desktop) so we can pin our own desktop version.
impl Source<'_> {
    pub(super) fn without_version(self) -> String {
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
pub(super) struct NormalizedSource<'a> {
    text: &'a mut String,
}

impl<'a> NormalizedSource<'a> {
    pub(super) fn new(text: &'a mut String) -> Self {
        Self { text }
    }

    pub(super) fn strip_precision(&mut self) {
        wreplace(self.text, "lowp", "");
        wreplace(self.text, "mediump", "");
        wreplace(self.text, "highp", "");
    }
}

/// Emit the data-uniform interface block at `binding = 0` (matching the frame's uniform bind entry). An
/// anonymous block puts its members in global scope so the shader body references them by their plain name.
/// The sampler texture/sampler bindings start at 1 ([`emit_sampler_decls`]) so the UBO never collides.
impl Declarations<'_> {
    pub(super) fn emit_uniform_block(out: &mut String, unis: &[Decl]) {
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
    pub(super) fn split_sampler(ty: &str) -> (&'static str, &'static str, &'static str) {
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
    pub(super) fn emit_sampler_decls(out: &mut String, samps: &[Decl]) {
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
    pub(super) fn rewrite_sampler_refs(body: &mut String, samps: &[Decl]) {
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
    pub(super) fn has_sampler_parameter(self) -> bool {
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
            if !consts.contains(&c) {
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
