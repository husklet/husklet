use super::*;

fn replace_identifier(source: &mut String, from: &str, to: &str) {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut at = 0usize;
    while at < bytes.len() {
        if Tokens::is_word(bytes[at]) {
            let start = at;
            while at < bytes.len() && Tokens::is_word(bytes[at]) {
                at += 1;
            }
            let word = &source[start..at];
            output.push_str(if word == from { to } else { word });
        } else {
            output.push(char::from(bytes[at]));
            at += 1;
        }
    }
    *source = output;
}

#[derive(Clone)]
struct Lexeme {
    start: usize,
    end: usize,
    text: String,
}

fn lexemes(source: &str) -> Vec<Lexeme> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        if bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' {
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
        } else {
            at += source[at..].chars().next().map_or(1, char::len_utf8);
        }
        out.push(Lexeme {
            start,
            end: at,
            text: source[start..at].to_string(),
        });
    }
    out
}

fn matching_lexeme(tokens: &[Lexeme], open: usize, left: &str, right: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        if token.text == left {
            depth += 1;
        } else if token.text == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(at);
            }
        }
    }
    None
}

/// Naga's desktop GLSL parser keeps structure types visible even after an ES value declaration shadows
/// the type. Consequently a later value use is parsed as a constructor. Give only the shadowing value a
/// private spelling; constructor/type occurrences retain the source spelling and semantics.
fn disambiguate_struct_value_shadows(source: &mut String) {
    let mut serial = 0usize;
    loop {
        let tokens = lexemes(source);
        let structs = tokens
            .windows(2)
            .filter(|pair| pair[0].text == "struct")
            .map(|pair| pair[1].text.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let Some((declaration, name)) = (1..tokens.len()).find_map(|at| {
            let name = &tokens[at].text;
            (structs.contains(name)
                && tokens[at - 1].text != "struct"
                && (structs.contains(&tokens[at - 1].text)
                    || matches!(
                        tokens[at - 1].text.as_str(),
                        "bool"
                            | "int"
                            | "uint"
                            | "float"
                            | "vec2"
                            | "vec3"
                            | "vec4"
                            | "ivec2"
                            | "ivec3"
                            | "ivec4"
                            | "uvec2"
                            | "uvec3"
                            | "uvec4"
                            | "bvec2"
                            | "bvec3"
                            | "bvec4"
                            | "mat2"
                            | "mat3"
                            | "mat4"
                    ))
                && tokens.get(at + 1).map(|token| token.text.as_str()) != Some("("))
            .then(|| (at, name.clone()))
        }) else {
            break;
        };

        let enclosing = (0..declaration).rev().find(|at| {
            tokens[*at].text == "{"
                && matching_lexeme(&tokens, *at, "{", "}").is_some_and(|close| declaration < close)
        });
        let close = if let Some(open) = enclosing {
            matching_lexeme(&tokens, open, "{", "}").unwrap()
        } else {
            let body = tokens[declaration..]
                .iter()
                .position(|token| token.text == "{")
                .map(|offset| declaration + offset);
            let Some(body) = body else { break };
            let Some(close) = matching_lexeme(&tokens, body, "{", "}") else {
                break;
            };
            close
        };
        let replacement = format!("hl_shadow_{name}_{serial}");
        serial += 1;
        let edits = (declaration..close)
            .filter(|at| {
                tokens[*at].text == name
                    && (*at == declaration
                        || (tokens.get(*at + 1).map(|token| token.text.as_str()) != Some("(")
                            && tokens
                                .get(at.wrapping_sub(1))
                                .map(|token| token.text.as_str())
                                != Some("struct")
                            && !tokens
                                .get(*at + 1)
                                .is_some_and(|next| Tokens::is_word(next.text.as_bytes()[0]))))
            })
            .map(|at| tokens[at].start..tokens[at].end)
            .collect::<Vec<_>>();
        for range in edits.into_iter().rev() {
            source.replace_range(range, &replacement);
        }
    }
}

/// Desktop GLSL/Naga does not parse the ES declaration form in a while condition. Lower it to the same
/// per-iteration initialization and lifetime: the condition variable is recreated before each test, is
/// visible in the body, and disappears when the loop finishes.
fn lower_while_condition_declarations(source: &mut String) {
    loop {
        let tokens = lexemes(source);
        let Some((while_at, open, close)) = tokens.iter().enumerate().find_map(|(at, token)| {
            if token.text != "while" || tokens.get(at + 1)?.text != "(" {
                return None;
            }
            let close = matching_lexeme(&tokens, at + 1, "(", ")")?;
            (tokens.get(at + 2).is_some_and(|token| {
                matches!(token.text.as_str(), "bool" | "int" | "uint" | "float")
            }) && tokens
                .get(at + 3)
                .is_some_and(|token| Tokens::is_word(token.text.as_bytes()[0]))
                && tokens.get(at + 4).is_some_and(|token| token.text == "="))
            .then_some((at, at + 1, close))
        }) else {
            break;
        };
        let body_start = close + 1;
        let Some(first_body) = tokens.get(body_start) else {
            break;
        };
        let body_end = if first_body.text == "{" {
            let Some(end) = matching_lexeme(&tokens, body_start, "{", "}") else {
                break;
            };
            end
        } else {
            let Some(offset) = tokens[body_start..]
                .iter()
                .position(|token| token.text == ";")
            else {
                break;
            };
            body_start + offset
        };
        let ty = &tokens[open + 1].text;
        let name = &tokens[open + 2].text;
        let expression = &source[tokens[open + 3].end..tokens[close].start];
        let body = if first_body.text == "{" {
            &source[first_body.end..tokens[body_end].start]
        } else {
            &source[first_body.start..tokens[body_end].end]
        };
        let replacement = format!(
            "{{ while (true) {{ {ty} {name} = {expression}; if (!{name}) break; {body} }} }}"
        );
        source.replace_range(tokens[while_at].start..tokens[body_end].end, &replacement);
    }
}

/// Naga hoists an unbraced declaration controlled by `if` into the enclosing function scope, leaving an
/// empty branch. Braces make the source-mandated single-statement lifetime explicit to the host parser.
fn brace_unbraced_if_declarations(source: &mut String) {
    loop {
        let tokens = lexemes(source);
        let structs = tokens
            .windows(2)
            .filter(|pair| pair[0].text == "struct")
            .map(|pair| pair[1].text.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let Some((body_start, body_end)) = tokens.iter().enumerate().find_map(|(at, token)| {
            if token.text != "if" || tokens.get(at + 1)?.text != "(" {
                return None;
            }
            let close = matching_lexeme(&tokens, at + 1, "(", ")")?;
            let body = close + 1;
            let ty = tokens.get(body)?.text.as_str();
            let declaration = structs.contains(ty)
                || matches!(
                    ty,
                    "bool"
                        | "int"
                        | "uint"
                        | "float"
                        | "vec2"
                        | "vec3"
                        | "vec4"
                        | "ivec2"
                        | "ivec3"
                        | "ivec4"
                        | "uvec2"
                        | "uvec3"
                        | "uvec4"
                        | "bvec2"
                        | "bvec3"
                        | "bvec4"
                        | "mat2"
                        | "mat3"
                        | "mat4"
                );
            if !declaration || tokens.get(body + 1)?.text == "(" {
                return None;
            }
            let end = tokens[body..].iter().position(|token| token.text == ";")? + body;
            Some((body, end))
        }) else {
            break;
        };
        source.insert(tokens[body_end].end, '}');
        source.insert(tokens[body_start].start, '{');
    }
}

// ---------------------------------------------------------------------------------------------------

/// GLSL-ES compute (`GL_COMPUTE_SHADER`) → desktop GLSL the host compiles. We FORWARD the source (the host
/// owns the compiler — naga on the wgpu executor) rather than pre-translating to a backend IR: strip
/// comments + any ES `#version … es` directive and pin a desktop `#version`, so naga's `glsl-in` accepts
/// it. The entry point stays `main` in-source and is renamed to the pipeline-bound `cmain` host-side. The
/// software oracle does not execute a GLSL compute payload (it runs only neutral KERNEL programs), so this
/// is asserted at the `Cmd` level; on wgpu it is a real compute module.
impl Translator {
    pub fn compute(cs_in: &str) -> String {
        let comments = Source::new(cs_in).expanded();
        let mut body = Source::new(&comments).without_version();
        NormalizedSource::new(&mut body).strip_precision();
        offset_compute_uniform_blocks(&mut body);
        let mut out = String::new();
        out.push_str(GLSL_VERSION);
        out.push_str(&body);
        out
    }
}

/// GL keeps uniform- and shader-storage-buffer binding indices in separate namespaces, while the host
/// descriptor set uses one binding namespace. Reserve the slots after all legal SSBO bindings for UBOs.
fn offset_compute_uniform_blocks(source: &mut String) {
    let original = source.clone();
    let bytes = original.as_bytes();
    let mut replacements = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = original[cursor..].find("uniform") {
        let uniform = cursor + relative;
        let before_word = uniform > 0 && Tokens::is_word(bytes[uniform - 1]);
        let after_word = uniform + 7 < bytes.len() && Tokens::is_word(bytes[uniform + 7]);
        cursor = uniform + 7;
        if before_word || after_word {
            continue;
        }
        let mut block = cursor;
        while block < bytes.len() && Tokens::is_space(bytes[block]) {
            block += 1;
        }
        while block < bytes.len() && Tokens::is_word(bytes[block]) {
            block += 1;
        }
        while block < bytes.len() && Tokens::is_space(bytes[block]) {
            block += 1;
        }
        if bytes.get(block) != Some(&b'{') {
            continue;
        }
        let declaration_start = original[..uniform]
            .rfind([';', '}'])
            .map_or(0, |position| position + 1);
        let prefix = &original[declaration_start..uniform];
        let Some(layout_relative) = prefix.rfind("layout") else {
            replacements.push((
                uniform..uniform,
                format!(
                    "layout(binding = {}) ",
                    crate::model::glconst::MAX_SHADER_STORAGE_BUFFER_BINDINGS
                ),
            ));
            continue;
        };
        let layout = declaration_start + layout_relative;
        let segment = &original[layout..uniform];
        if let Some(binding_relative) = segment.find("binding") {
            let after_binding = layout + binding_relative + "binding".len();
            let digit_start = original[after_binding..uniform]
                .find(|character: char| character.is_ascii_digit())
                .map(|relative| after_binding + relative);
            if let Some(digit_start) = digit_start {
                let digit_end = original[digit_start..uniform]
                    .find(|character: char| !character.is_ascii_digit())
                    .map_or(uniform, |relative| digit_start + relative);
                if let Ok(binding) = original[digit_start..digit_end].parse::<u32>() {
                    replacements.push((
                        digit_start..digit_end,
                        (crate::model::glconst::MAX_SHADER_STORAGE_BUFFER_BINDINGS + binding)
                            .to_string(),
                    ));
                }
            }
        } else if let Some(close) = segment.rfind(')') {
            replacements.push((
                layout + close..layout + close,
                format!(
                    ", binding = {}",
                    crate::model::glconst::MAX_SHADER_STORAGE_BUFFER_BINDINGS
                ),
            ));
        }
    }
    for (range, replacement) in replacements.into_iter().rev() {
        source.replace_range(range, &replacement);
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
    /// Premultiply every fragment output's RGB channels immediately before `main` returns.
    ///
    /// This is used when GL fixed-function blending names both `GL_CONSTANT_COLOR` and
    /// `GL_CONSTANT_ALPHA` for the RGB source/destination pair. WebGPU has one constant spelling, so the
    /// source factor is folded into the shader output and fixed-function blending keeps the destination
    /// factor. Alpha is deliberately untouched.
    pub(crate) fn scale_fragment_outputs(self, scale: [f32; 3]) -> String {
        let source = self.text;
        let outputs = Tokens(source).collect("out");
        if outputs.is_empty() {
            return source.to_string();
        }
        let Some(main) = source.find("void main") else {
            return source.to_string();
        };
        let scale = format!("vec3({:.9}, {:.9}, {:.9})", scale[0], scale[1], scale[2]);
        let mut wrapper = String::from("\nvoid main() {\n hl_blend_main();");
        for output in outputs {
            wrapper.push_str(&format!(
                "\n {}.rgb = clamp({}.rgb, vec3(0.0), vec3(1.0)) * {scale};",
                output.name, output.name
            ));
        }
        wrapper.push_str("\n}\n");
        let mut rewritten = source.to_string();
        let name = main + "void ".len();
        rewritten.replace_range(name..name + "main".len(), "hl_blend_main");
        rewritten.push_str(&wrapper);
        rewritten
    }

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

// ES → desktop source normalisation (precision qualifiers, texture builtins) lives in `normalize`.

/// Emit the data-uniform interface block at `binding = 0` (matching the frame's uniform bind entry). An
/// anonymous block puts its members in global scope so the shader body references them by their plain name.
/// The sampler texture/sampler bindings start at 1 ([`emit_sampler_decls`]) so the UBO never collides.
impl Declarations<'_> {
    pub(super) fn emit_uniform_layout(out: &mut String, unis: &[Uni]) {
        if unis.is_empty() {
            return;
        }
        out.push_str("layout(std140, binding = 0) uniform HlUniforms {\n");
        let mut cursor = 0usize;
        let mut padding = 0usize;
        for u in unis {
            let offset = u.off.max(0) as usize;
            while cursor < offset {
                out.push_str(&format!("    uint _hlpad{padding};\n"));
                cursor += 4;
                padding += 1;
            }
            let name = Self::storage_name(&u.name);
            if let Some((columns, _)) = matrix_shape(&u.ty).filter(|_| name != u.name) {
                for element in 0..u.arr.max(1) {
                    for column in 0..columns {
                        out.push_str(&format!("    vec4 {name}_hle{element}_hlc{column};\n"));
                    }
                }
            } else if u.arr > 0 {
                out.push_str(&format!(
                    "    {} {}[{}];\n",
                    Self::storage_type(&u.ty),
                    name,
                    u.arr
                ));
            } else {
                out.push_str(&format!("    {} {};\n", Self::storage_type(&u.ty), name));
            }
            cursor = offset.saturating_add(u.sz.max(0) as usize);
        }
        out.push_str("};\n");
    }

    pub(super) fn emit_uniform_block(out: &mut String, unis: &[Decl]) {
        if unis.is_empty() {
            return;
        }
        out.push_str("layout(std140, binding = 0) uniform HlUniforms {\n");
        for uniform in unis {
            if uniform.arr > 0 {
                out.push_str(&format!(
                    "    {} {}[{}];\n",
                    uniform.ty, uniform.name, uniform.arr
                ));
            } else {
                out.push_str(&format!("    {} {};\n", uniform.ty, uniform.name));
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
        sampler_split(ty).expect("sampler declarations use the TypeToken sampler vocabulary")
    }

    /// Emit each combined image-sampler as a SEPARATE `texture2D` + `sampler` pair (naga rejects a combined
    /// `uniform sampler2D`). The uniform block owns binding 0; sampler `k` (declaration index) owns TEXTURE
    /// binding `1 + 2k` and SAMPLER binding `2 + 2k` — every UBO/texture/sampler thus lands on a DISTINCT
    /// binding within the single wgpu bind-group namespace, exactly matching the `BindEntry`s
    /// [`crate::service::frame::build_frame_ir`] emits. The shader body recombines the pair at each use via
    /// [`rewrite_sampler_refs`].
    pub(super) fn emit_sampler_decls(out: &mut String, samps: &[Decl]) {
        let mut k = 0usize;
        for s in samps {
            let (tex_ty, smp_ty, _) = Self::split_sampler(&s.ty);
            for element in 0..s.arr.max(1) {
                let name = Self::sampler_element_name(s, element);
                let tex_binding = 1 + 2 * k;
                let smp_binding = 2 + 2 * k;
                out.push_str(&format!(
                    "layout(binding = {tex_binding}) uniform {tex_ty} {name}_hltex;\n"
                ));
                out.push_str(&format!(
                    "layout(binding = {smp_binding}) uniform {smp_ty} {name}_hlsmp;\n"
                ));
                k += 1;
            }
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
            // An INTEGER sampler never recombines: there is no legal operation that pairs it with a
            // sampler, so every remaining use — a `texelFetch` or `textureSize` the guest wrote itself,
            // and anything [`rewrite_integer_sampler_fetches`] already converted — wants the bare texture
            // global. Emitting the constructor here would hand a sampled-image to a fetch instruction.
            let integer = TypeToken(&s.ty).is_integer_sampler();
            if s.arr > 0 {
                for element in (0..s.arr).rev() {
                    let source = format!("{}[{element}]", s.name);
                    let name = Self::sampler_element_name(s, element);
                    let replacement = if integer {
                        format!("{name}_hltex")
                    } else {
                        format!("{ctor}({name}_hltex, {name}_hlsmp)")
                    };
                    wreplace(body, &source, &replacement);
                }
            } else {
                let name = Self::sampler_element_name(s, 0);
                let repl = if integer {
                    format!("{name}_hltex")
                } else {
                    format!("{ctor}({name}_hltex, {name}_hlsmp)")
                };
                wreplace(body, &s.name, &repl);
            }
        }
    }

    /// Rewrite every `texture(NAME, COORD)` / `texture2D(NAME, COORD)` on an INTEGER sampler into
    /// `texelFetch(NAME_hltex, ivec2(COORD * vec2(textureSize(NAME_hltex, 0))), 0)`.
    ///
    /// An integer texture cannot be SAMPLED: it has no normalized reading, so it cannot be filtered, and a
    /// bind group that offers one to a sampling instruction is refused by the backend outright. `texelFetch`
    /// is the only legal access, and it addresses texels by integer index — hence the multiply by
    /// `textureSize`, which turns the normalized coordinate the guest wrote into that index.
    ///
    /// GL_NEAREST is the only filter an integer texture may carry (ES 3.0 §3.8.13), so truncating the
    /// scaled coordinate reproduces exactly what sampling it would have selected.
    ///
    /// Runs BEFORE [`rewrite_sampler_refs`], which then finds no bare occurrence of the name left to
    /// recombine — `NAME_hltex` is one word, so its word-boundary replace cannot match inside it.
    pub(super) fn rewrite_integer_sampler_fetches(body: &mut String, samps: &[Decl]) {
        for s in samps {
            if !TypeToken(&s.ty).is_integer_sampler() {
                continue;
            }
            for element in (0..s.arr.max(1)).rev() {
                let source = if s.arr == 0 {
                    s.name.clone()
                } else {
                    format!("{}[{element}]", s.name)
                };
                let name = Self::sampler_element_name(s, element);
                while let Some((start, coord, end)) = Self::find_texture_call(body, &source) {
                    let fetch = format!(
                        "texelFetch({name}_hltex, ivec2(({coord}) * vec2(textureSize({name}_hltex, 0))), 0)"
                    );
                    body.replace_range(start..end, &fetch);
                }
            }
        }
    }

    /// Locate one `texture(`/`texture2D(` call whose FIRST argument is exactly `sampler`, returning the
    /// call's byte range and its coordinate argument. Paren-balanced, so a coordinate containing its own
    /// calls or parentheses is extracted whole.
    fn find_texture_call(body: &str, sampler: &str) -> Option<(usize, String, usize)> {
        for call in ["texture2D(", "texture("] {
            let mut from = 0usize;
            while let Some(offset) = body[from..].find(call) {
                let start = from + offset;
                let before_is_word = start > 0 && Tokens::is_word(body.as_bytes()[start - 1]);
                let open = start + call.len();
                let rest = body[open..].trim_start();
                let lead = body[open..].len() - rest.len();
                if before_is_word || !rest.starts_with(sampler) {
                    from = start + call.len();
                    continue;
                }
                let after_name = open + lead + sampler.len();
                let tail = body[after_name..].trim_start();
                if !tail.starts_with(',') {
                    from = start + call.len();
                    continue;
                }
                let comma = after_name + (body[after_name..].len() - tail.len());
                // Balance from the opening paren to find the call's end.
                let bytes = body.as_bytes();
                let (mut depth, mut index) = (1i32, open);
                while index < bytes.len() && depth > 0 {
                    match bytes[index] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    index += 1;
                }
                if depth != 0 {
                    return None;
                }
                let coord = body[comma + 1..index].trim().to_owned();
                return Some((start, coord, index + 1));
            }
        }
        None
    }

    fn sampler_element_name(sampler: &Decl, element: u32) -> String {
        let base = Self::storage_name(&sampler.name);
        if sampler.arr == 0 {
            base
        } else {
            format!("{base}_{element}")
        }
    }

    fn storage_name(source: &str) -> String {
        source
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn storage_type(source: &str) -> &str {
        match source {
            "bool" => "uint",
            "bvec2" => "uvec2",
            "bvec3" => "uvec3",
            "bvec4" => "uvec4",
            _ => source,
        }
    }

    fn rewrite_data_refs(body: &mut String, uniforms: &[Uni]) {
        Self::rewrite_data_refs_except(body, uniforms, &[]);
    }

    fn rewrite_data_refs_except(body: &mut String, uniforms: &[Uni], aggregate_roots: &[&str]) {
        for uniform in uniforms.iter().rev() {
            if aggregate_roots.iter().any(|root| {
                uniform.name == *root
                    || uniform
                        .name
                        .strip_prefix(root)
                        .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('['))
            }) {
                continue;
            }
            let replacement = Self::storage_name(&uniform.name);
            if let Some((columns, rows)) =
                matrix_shape(&uniform.ty).filter(|_| replacement != uniform.name)
            {
                let swizzle = match rows {
                    2 => ".xy",
                    3 => ".xyz",
                    _ => "",
                };
                for element in (0..uniform.arr.max(1)).rev() {
                    let source = if uniform.arr > 0 {
                        format!("{}[{element}]", uniform.name)
                    } else {
                        uniform.name.clone()
                    };
                    let columns = (0..columns)
                        .map(|column| format!("{replacement}_hle{element}_hlc{column}{swizzle}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    wreplace(body, &source, &format!("{}({columns})", uniform.ty));
                }
                continue;
            }
            if matches!(uniform.ty.as_str(), "bool" | "bvec2" | "bvec3" | "bvec4") {
                for element in (0..uniform.arr.max(1)).rev() {
                    let source = if uniform.arr > 0 {
                        format!("{}[{element}]", uniform.name)
                    } else {
                        uniform.name.clone()
                    };
                    let stored = if uniform.arr > 0 {
                        format!("{replacement}[{element}]")
                    } else {
                        replacement.clone()
                    };
                    let value = if uniform.ty == "bool" {
                        format!("({stored} != 0u)")
                    } else {
                        format!(
                            "notEqual({stored}, {}(0u))",
                            Self::storage_type(&uniform.ty)
                        )
                    };
                    wreplace(body, &source, &value);
                }
                continue;
            }
            if replacement != uniform.name {
                wreplace(body, &uniform.name, &replacement);
            }
        }
    }

    fn emit_data_aggregate_globals(out: &mut String, aggregates: &[DataAggregate]) {
        for aggregate in aggregates {
            let declaration = &aggregate.declaration;
            if declaration.arr > 0 {
                out.push_str(&format!(
                    "{} {}[{}];\n",
                    declaration.ty, declaration.name, declaration.arr
                ));
            } else {
                out.push_str(&format!("{} {};\n", declaration.ty, declaration.name));
            }
        }
    }

    fn data_aggregate_initializers(aggregates: &[DataAggregate], uniforms: &[Uni]) -> String {
        let mut out = String::new();
        for aggregate in aggregates {
            for leaf in &aggregate.leaves {
                let Some(uniform) = uniforms
                    .iter()
                    .find(|uniform| uniform.name == leaf.declaration.name)
                else {
                    continue;
                };
                for target in leaf.targets() {
                    let mut value = target.clone();
                    Self::rewrite_data_refs(&mut value, std::slice::from_ref(uniform));
                    out.push_str(&format!("{target} = {value};\n"));
                }
            }
        }
        out
    }
}

impl Source<'_> {
    /// Expand sampler-array declarations into one separately-bound sampler per element and rewrite constant
    /// element references. This is used after verbatim-stage preparation so the host bind-group model sees
    /// the same flattened resources as GL reflection/lowering.
    pub fn expand_sampler_arrays(self) -> String {
        let declarations = Declarations::from_stages(self.text, "").uniforms().1;
        let arrays = declarations
            .into_iter()
            .filter(|declaration| declaration.arr > 0 && declaration.array_literal)
            .collect::<Vec<_>>();
        if arrays.is_empty() {
            return self.text.to_owned();
        }

        let bytes = self.text.as_bytes();
        let mut edits = Vec::<(usize, usize, String)>::new();
        let mut cursor = 0usize;
        while let Some(at) = find_from(bytes, b"uniform", cursor) {
            cursor = at + "uniform".len();
            if (at > 0 && Tokens::is_word(bytes[at - 1]))
                || (cursor < bytes.len() && Tokens::is_word(bytes[cursor]))
            {
                continue;
            }
            let mut q = cursor;
            while q < bytes.len() && Tokens::is_space(bytes[q]) {
                q += 1;
            }
            let word = |at: &mut usize| {
                let start = *at;
                while *at < bytes.len() && Tokens::is_word(bytes[*at]) {
                    *at += 1;
                }
                &self.text[start..*at]
            };
            let mut ty = word(&mut q);
            while TypeToken(ty).is_precision() {
                while q < bytes.len() && Tokens::is_space(bytes[q]) {
                    q += 1;
                }
                ty = word(&mut q);
            }
            while q < bytes.len() && Tokens::is_space(bytes[q]) {
                q += 1;
            }
            let name = word(&mut q);
            let Some(declaration) = arrays
                .iter()
                .find(|declaration| declaration.name == name && declaration.ty == ty)
            else {
                continue;
            };
            while q < bytes.len() && Tokens::is_space(bytes[q]) {
                q += 1;
            }
            if q >= bytes.len() || bytes[q] != b'[' {
                continue;
            }
            let Some(relative_end) = bytes[q..].iter().position(|&byte| byte == b';') else {
                continue;
            };
            let end = q + relative_end + 1;
            let mut replacement = String::new();
            for element in 0..declaration.arr {
                replacement.push_str(&format!(
                    "uniform {} {}_{};\n",
                    declaration.ty, declaration.name, element
                ));
            }
            edits.push((at, end, replacement));
            cursor = end;
        }

        let mut output = self.text.to_owned();
        for (start, end, replacement) in edits.into_iter().rev() {
            output.replace_range(start..end, &replacement);
        }
        Declarations::rewrite_integer_sampler_fetches(&mut output, &arrays);
        Declarations::rewrite_sampler_refs(&mut output, &arrays);
        output
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
        let src = self.expanded();
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
        self.translate_render_with(&std::collections::BTreeMap::new())
    }

    pub fn translate_render_with(
        self,
        attribute_bindings: &std::collections::BTreeMap<String, u32>,
    ) -> (String, String) {
        let mut vs = Source::new(self.vertex).expanded();
        let mut fs = Source::new(self.fragment).expanded();
        disambiguate_struct_value_shadows(&mut vs);
        disambiguate_struct_value_shadows(&mut fs);
        super::types::StructSamplers::lower(&mut vs);
        super::types::StructSamplers::lower(&mut fs);
        super::types::StructEquality::lower(&mut vs);
        super::types::StructEquality::lower(&mut fs);
        lower_while_condition_declarations(&mut vs);
        lower_while_condition_declarations(&mut fs);
        brace_unbraced_if_declarations(&mut vs);
        brace_unbraced_if_declarations(&mut fs);
        let invariant_position = Source::new(&vs).has_invariant_position();
        let declares_modern_es = |source: &str| {
            source.lines().any(|line| {
                let version = line.trim_start();
                version.starts_with("#version") && !version.starts_with("#version 100")
            })
        };
        let strict_es100_calls = !declares_modern_es(&vs) && !declares_modern_es(&fs);

        let attrs = Source::new(&vs).vertex_attrs();
        // GLES 2 permits multiple attribute names to alias one location when the shader does not execute
        // paths which consume both. Naga cannot represent duplicate input locations. When the declarations
        // have the same shape they are the same physical input, so retain one declaration and rewrite the
        // other names to it. Differently shaped aliases still need distinct host locations from the linker.
        let mut alias_canonical = std::collections::BTreeMap::<String, String>::new();
        for (index, attribute) in attrs.iter().enumerate() {
            let Some(location) = attribute_bindings.get(&attribute.name) else {
                continue;
            };
            if let Some(canonical) = attrs[..index].iter().find(|candidate| {
                candidate.ty == attribute.ty
                    && candidate.arr == attribute.arr
                    && attribute_bindings.get(&candidate.name) == Some(location)
            }) {
                alias_canonical.insert(attribute.name.clone(), canonical.name.clone());
            }
        }
        let mut vary = Tokens(&vs).collect("varying");
        vary.truncate(16);
        append_decls_unique(&mut vary, Tokens(&vs).collect("out"), 16);
        let unis = StageSources::new(&vs, &fs)
            .uniform_layout()
            .map(|(uniforms, _)| uniforms)
            .unwrap_or_default();
        let samps = Declarations::from_stages(&vs, &fs).uniforms().1;
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
        let uniform_structs = Declarations::from_stages(&vs, &fs).uniform_structs();
        let data_uniform_structs = Declarations::from_stages(&vs, &fs).data_uniform_structs();
        let vs_aggregates = Declarations::from_stages(&vs, "").data_aggregates();
        let fs_aggregates = Declarations::from_stages(&fs, "").data_aggregates();
        let rewrite = |items: &[String], samps: &[Decl], aggregates: &[DataAggregate]| -> String {
            let mut out = String::new();
            let roots = aggregates
                .iter()
                .map(|aggregate| aggregate.declaration.name.as_str())
                .collect::<Vec<_>>();
            for it in items {
                let mut words = it
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .filter(|word| !word.is_empty());
                if words.next() == Some("struct")
                    && words.next().is_some_and(|name| {
                        uniform_structs.contains(name) && !data_uniform_structs.contains(name)
                    })
                {
                    // Opaque aggregate uniforms are lowered to standalone texture/sampler bindings below.
                    // Desktop GLSL/naga does not admit a combined sampler type inside a structure even when
                    // that structure is otherwise unused, so do not carry the now-unreferenced type through.
                    continue;
                }
                let mut t = it.clone();
                NormalizedSource::new(&mut t).strip_precision();
                Declarations::rewrite_data_refs_except(&mut t, &unis, &roots);
                Declarations::rewrite_integer_sampler_fetches(&mut t, samps);
                Declarations::rewrite_sampler_refs(&mut t, samps);
                NormalizedSource::new(&mut t).lower_texture_builtins();
                out.push_str(&t);
                out.push('\n');
            }
            out
        };

        // ---- vertex stage ----
        let mut vs_out = String::new();
        vs_out.push_str(GLSL_VERSION);
        if strict_es100_calls {
            vs_out.push_str("#define HL_GLSL_ES100 1\n");
        }
        if invariant_position {
            vs_out.push_str("invariant gl_Position;\n");
        }
        for c in &consts {
            vs_out.push_str(c);
            vs_out.push('\n');
        }
        vs_out.push_str(&rewrite(&vs_structs, &samps, &vs_aggregates));
        // Locations are allocated by SPAN, not by declaration: a matrix occupies one location per column
        // and an array one per element, so numbering them sequentially makes the next declaration overlap
        // the previous one. naga rejects that as `BindingCollision`, and the program then links, draws
        // with `GL_NO_ERROR`, paints nothing and wedges the context.
        let mut used = std::collections::BTreeSet::new();
        for (name, at) in attribute_bindings {
            let span = attrs
                .iter()
                .find(|a| &a.name == name)
                .map_or(1, |a| a.location_span());
            used.extend(*at..at.saturating_add(span));
        }
        for a in &attrs {
            if alias_canonical.contains_key(&a.name) {
                continue;
            }
            let location = attribute_bindings
                .get(&a.name)
                .copied()
                .unwrap_or_else(|| Declarations::reserve_run(&mut used, a.location_span()));
            let name = if a.arr > 0 {
                format!("{}[{}]", a.name, a.arr)
            } else {
                a.name.clone()
            };
            vs_out.push_str(&format!(
                "layout(location = {location}) in {} {};\n",
                a.ty, name
            ));
        }
        // Varyings are numbered by span for the same reason, and the fragment stage below repeats this
        // walk so the two stages agree declaration for declaration.
        let mut varying_location = 0u32;
        for v in vary.iter() {
            let flat = if v.requires_flat_interpolation() {
                "flat "
            } else {
                ""
            };
            let name = if v.arr > 0 {
                format!("{}[{}]", v.name, v.arr)
            } else {
                v.name.clone()
            };
            vs_out.push_str(&format!(
                "layout(location = {varying_location}) {flat}out {} {};\n",
                v.ty, name
            ));
            varying_location += v.location_span();
        }
        Declarations::emit_uniform_layout(&mut vs_out, &unis);
        Declarations::emit_sampler_decls(&mut vs_out, &samps);
        Declarations::emit_data_aggregate_globals(&mut vs_out, &vs_aggregates);
        vs_out.push_str(&rewrite(&vs_funcs, &samps, &vs_aggregates));
        // `Program::link` refuses a stage whose body cannot be found, so this cannot be reached with a
        // real program. Kept honest rather than silent: regenerating an empty body from a shader that HAS
        // one is the wrong-render defect, and it must not come back through a caller that skips the gate.
        let mut vb = Source::new(&vs).main_body().unwrap_or_else(|| {
            hl_log::hl_error!(
                hl_log::tag::GL,
                "glsl vs translate: no findable main body — emitting an EMPTY main, which compiles \
                 and draws NOTHING. The link gate should have refused this."
            );
            String::new()
        });
        for (alias, canonical) in &alias_canonical {
            replace_identifier(&mut vb, alias, canonical);
        }
        NormalizedSource::new(&mut vb).strip_precision();
        let vs_roots = vs_aggregates
            .iter()
            .map(|aggregate| aggregate.declaration.name.as_str())
            .collect::<Vec<_>>();
        Declarations::rewrite_data_refs_except(&mut vb, &unis, &vs_roots);
        Declarations::rewrite_integer_sampler_fetches(&mut vb, &samps);
        Declarations::rewrite_sampler_refs(&mut vb, &samps);
        NormalizedSource::new(&mut vb).lower_texture_builtins();
        let vs_initializers = Declarations::data_aggregate_initializers(&vs_aggregates, &unis);
        vs_out.push_str(&format!("void main() {{\n{vs_initializers}{vb}\n}}\n"));
        NormalizedSource::new(&mut vs_out).pin_vertex_lod();

        // ---- fragment stage ----
        let mut fs_out = String::new();
        fs_out.push_str(GLSL_VERSION);
        if strict_es100_calls {
            fs_out.push_str("#define HL_GLSL_ES100 1\n");
        }
        for c in &consts {
            fs_out.push_str(c);
            fs_out.push('\n');
        }
        fs_out.push_str(&rewrite(&fs_structs, &samps, &fs_aggregates));
        // The SAME span walk as the vertex stage: the two must agree declaration for declaration, or the
        // inter-stage interface stops matching at the first multi-location varying.
        let mut varying_location = 0u32;
        for v in vary.iter() {
            let flat = if v.requires_flat_interpolation() {
                "flat "
            } else {
                ""
            };
            let name = if v.arr > 0 {
                format!("{}[{}]", v.name, v.arr)
            } else {
                v.name.clone()
            };
            fs_out.push_str(&format!(
                "layout(location = {varying_location}) {flat}in {} {};\n",
                v.ty, name
            ));
            varying_location += v.location_span();
        }
        Declarations::emit_uniform_layout(&mut fs_out, &unis);
        Declarations::emit_sampler_decls(&mut fs_out, &samps);
        Declarations::emit_data_aggregate_globals(&mut fs_out, &fs_aggregates);
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
        } else if let Some(output) = fragouts.first() {
            fs_out.push_str(&format!(
                "layout(location = 0) out {} {frag_name};\n",
                output.ty
            ));
        } else {
            fs_out.push_str(&format!("layout(location = 0) out vec4 {frag_name};\n"));
        }
        // `Program::link` refuses a stage whose body cannot be found, so this cannot be reached with a
        // real program. Kept honest rather than silent: regenerating an empty body from a shader that HAS
        // one is the wrong-render defect, and it must not come back through a caller that skips the gate.
        let mut fb = Source::new(&fs).main_body().unwrap_or_else(|| {
            hl_log::hl_error!(
                hl_log::tag::GL,
                "glsl fs translate: no findable main body — emitting an EMPTY main, which compiles \
                 and draws NOTHING. The link gate should have refused this."
            );
            String::new()
        });
        NormalizedSource::new(&mut fb).strip_precision();
        let fs_roots = fs_aggregates
            .iter()
            .map(|aggregate| aggregate.declaration.name.as_str())
            .collect::<Vec<_>>();
        Declarations::rewrite_data_refs_except(&mut fb, &unis, &fs_roots);
        Declarations::rewrite_integer_sampler_fetches(&mut fb, &samps);
        Declarations::rewrite_sampler_refs(&mut fb, &samps);
        NormalizedSource::new(&mut fb).lower_texture_builtins();
        if fragouts.is_empty() {
            wreplace(&mut fb, "gl_FragColor", &frag_name);
            NormalizedSource::new(&mut fb).lower_single_output_frag_data(&frag_name);
        }
        fs_out.push_str(&rewrite(&fs_funcs, &samps, &fs_aggregates));
        let fs_initializers = Declarations::data_aggregate_initializers(&fs_aggregates, &unis);
        fs_out.push_str(&format!("void main() {{\n{fs_initializers}{fb}\n}}\n"));

        (vs_out, fs_out)
    }
}

/// Rewrite texture-coordinate arguments for selected samplers from GL's bottom-left render-target
/// convention to the host GPU's top-left convention. Uploaded texture planes retain their existing
/// orientation; only samplers backed by a rendered FBO are named by the caller.
impl Source<'_> {
    /// Convert OpenGL clip-space Y into the row orientation used by a directly-presented host texture.
    ///
    /// Offscreen framebuffer shaders must not use this conversion: their OpenGL texture orientation is
    /// preserved at sampling time. The frame lowerer specializes vertex modules by target kind.
    pub(crate) fn present_coordinates(self) -> String {
        self.append_to_main(" gl_Position.y = -gl_Position.y;")
    }

    /// Map OpenGL's clip volume onto the host GPU's.
    ///
    /// GL clips to `-w <= z <= w`; Metal and WebGPU clip to `0 <= z <= w`. Without this, every vertex
    /// with negative clip z is clipped away before rasterization — and since every standard projection
    /// matrix puts near geometry at negative clip z, an application loses its near half with depth
    /// testing not even enabled. This is a property of the clip volume, not of orientation, so unlike
    /// [`Self::present_coordinates`] it applies to EVERY vertex shader, offscreen included.
    ///
    /// `z_host = (z_gl + w) / 2` is the standard remap and is exact at both ends: `z = -w` maps to 0 and
    /// `z = +w` maps to `w`. It composes with the depth range the rasterizer applies afterwards.
    pub(crate) fn clip_depth(self) -> String {
        self.append_to_main(" gl_Position.z = (gl_Position.z + gl_Position.w) * 0.5;")
    }

    /// Insert `statement` as the last statement of `main`, or return the source untouched when there is
    /// no `main` body to append to.
    fn append_to_main(self, statement: &str) -> String {
        let source = self.text;
        let Some(main) = source.find("void main") else {
            return source.to_owned();
        };
        let Some(open) = source[main..].find('{').map(|at| main + at) else {
            return source.to_owned();
        };
        let mut depth = 0usize;
        let mut close = None;
        for (relative, byte) in source.as_bytes()[open..].iter().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + relative);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            return source.to_owned();
        };
        let mut rewritten = source.to_owned();
        rewritten.insert_str(close, &format!("\n{statement}\n"));
        rewritten
    }

    /// Restore OpenGL fragment-coordinate semantics after Naga maps the fragment position builtin onto
    /// WebGPU's top-left framebuffer coordinates.
    pub(crate) fn fragment_coordinates(
        self,
        target_height: i32,
        origin_upper_left: bool,
        pixel_center_integer: bool,
    ) -> String {
        let source = self.text;
        if !source.contains("gl_FragCoord")
            || (origin_upper_left && !pixel_center_integer)
            || target_height <= 0
        {
            return source.to_string();
        }
        let Some(main) = source.find("void main") else {
            return source.to_string();
        };
        let Some(open) = source[main..].find('{').map(|at| main + at) else {
            return source.to_string();
        };

        let mut rewritten = source.to_string();
        wreplace(&mut rewritten, "gl_FragCoord", "hl_FragCoord");
        let declaration = "vec4 hl_FragCoord;\n";
        rewritten.insert_str(main, declaration);
        let open = open + declaration.len();
        let x = if pixel_center_integer {
            "gl_FragCoord.x - 0.5"
        } else {
            "gl_FragCoord.x"
        };
        let y = match (origin_upper_left, pixel_center_integer) {
            (true, true) => "gl_FragCoord.y - 0.5".to_string(),
            (true, false) => "gl_FragCoord.y".to_string(),
            (false, true) => {
                format!("{target_height}.0 - gl_FragCoord.y - 0.5")
            }
            (false, false) => format!("{target_height}.0 - gl_FragCoord.y"),
        };
        rewritten.insert_str(
            open + 1,
            &format!("\n hl_FragCoord = vec4({x}, {y}, gl_FragCoord.z, gl_FragCoord.w);\n"),
        );
        rewritten
    }

    #[cfg(test)]
    pub(crate) fn flip_render_target_samplers(self, samplers: &[String]) -> String {
        let transforms = samplers
            .iter()
            .map(|sampler| {
                (
                    sampler.clone(),
                    true,
                    [
                        crate::model::glconst::GL_RED,
                        crate::model::glconst::GL_GREEN,
                        crate::model::glconst::GL_BLUE,
                        crate::model::glconst::GL_ALPHA,
                    ],
                )
            })
            .collect::<Vec<_>>();
        self.transform_texture_samplers(&transforms)
    }

    /// Apply per-texture coordinate orientation and component swizzle to normalized sampler calls.
    ///
    /// WGPU texture views do not expose OpenGL's per-object component mapping, so the GL boundary lowers
    /// non-identity mappings into a small fragment helper. The sampled value is evaluated once.
    pub(crate) fn transform_texture_samplers(
        self,
        transforms: &[(String, bool, [u32; 4])],
    ) -> String {
        use crate::model::glconst::{GL_ALPHA, GL_BLUE, GL_GREEN, GL_ONE, GL_RED, GL_ZERO};

        let source = self.text;
        if transforms.is_empty() {
            return source.to_string();
        }
        let identity = [GL_RED, GL_GREEN, GL_BLUE, GL_ALPHA];
        let helper = |index: usize| format!("hl_swizzle_{index}");
        let component = |value| match value {
            GL_RED => "value.r",
            GL_GREEN => "value.g",
            GL_BLUE => "value.b",
            GL_ALPHA => "value.a",
            GL_ZERO => "0.0",
            GL_ONE => "1.0",
            _ => "0.0",
        };
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0;
        while let Some((relative, function)) = ["texture(", "texture2D("]
            .into_iter()
            .filter_map(|function| source[cursor..].find(function).map(|at| (at, function)))
            .min_by_key(|(at, _)| *at)
        {
            let call = cursor + relative;
            let open = call + function.len() - 1;
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
            if let Some((index, (_, flip_y, swizzle))) = transforms
                .iter()
                .enumerate()
                .find(|(_, (name, _, _))| Tokens(sampler).names_sampler(name))
            {
                if *swizzle != identity {
                    let prefix_len = function.len();
                    out.truncate(out.len() - prefix_len);
                    out.push_str(&helper(index));
                    out.push('(');
                    out.push_str(function);
                }
                let coord_end = commas.get(1).copied().unwrap_or(close);
                let coord = source[first_comma + 1..coord_end].trim();
                out.push_str(&source[open + 1..first_comma + 1]);
                if *flip_y {
                    out.push_str(" vec2((");
                    out.push_str(coord);
                    out.push_str(").x, 1.0 - (");
                    out.push_str(coord);
                    out.push_str(").y)");
                } else {
                    out.push_str(&source[first_comma + 1..coord_end]);
                }
                out.push_str(&source[coord_end..=close]);
                if *swizzle != identity {
                    out.push(')');
                }
            } else {
                out.push_str(&source[open + 1..=close]);
            }
            cursor = close + 1;
        }
        out.push_str(&source[cursor..]);

        let mut helpers = String::new();
        for (index, (_, _, swizzle)) in transforms.iter().enumerate() {
            if *swizzle == identity {
                continue;
            }
            helpers.push_str("vec4 ");
            helpers.push_str(&helper(index));
            helpers.push_str("(vec4 value) { return vec4(");
            for (component_index, mapping) in swizzle.iter().enumerate() {
                if component_index != 0 {
                    helpers.push_str(", ");
                }
                helpers.push_str(component(*mapping));
            }
            helpers.push_str("); }\n");
        }
        if helpers.is_empty() {
            return out;
        }
        if let Some(main) = out.find("void main") {
            out.insert_str(main, &helpers);
        } else {
            out.push_str(&helpers);
        }
        out
    }
}

#[cfg(test)]
mod orientation_tests {
    use super::{Source, StageSources, Translator};

    #[test]
    fn translated_interfaces_preserve_array_declarators() {
        let vertex = "#version 300 es\nin vec4 position; out float values[7]; void main(){ gl_Position=position; values[0]=1.0; }";
        let fragment = "#version 300 es\nprecision highp float; in float values[7]; out vec4 color; void main(){ color=vec4(values[0]); }";
        let (vertex, fragment) = StageSources::new(vertex, fragment).translate_render();
        assert!(vertex.contains("out float values[7];"), "{vertex}");
        assert!(fragment.contains("in float values[7];"), "{fragment}");
    }

    #[test]
    fn single_integer_fragment_output_preserves_its_scalar_class() {
        let vertex = "attribute vec4 position; void main() { gl_Position = position; }";
        for (ty, value) in [("ivec4", "ivec4(1)"), ("uvec4", "uvec4(1u)")] {
            let fragment = format!(
                "#version 300 es\nprecision highp float;\nlayout(location = 0) out {ty} color;\nvoid main() {{ color = {value}; }}"
            );
            let (_, translated) = StageSources::new(vertex, &fragment).translate_render();
            assert!(
                translated.contains(&format!("layout(location = 0) out {ty} color;")),
                "{translated}"
            );
        }
    }

    /// GL clips to `-w <= z <= w`, the host to `0 <= z <= w`. Without the remap every vertex at
    /// negative clip z is clipped away before rasterization, so a full-screen triangle at
    /// `gl_Position.z = -0.5` paints nothing while `+0.5` paints correctly — with depth testing not even
    /// enabled. Every standard projection matrix puts near geometry at negative clip z.
    #[test]
    fn clip_depth_maps_the_gl_clip_volume_onto_the_host_volume() {
        let source = "void main() { gl_Position = vec4(x, y, -0.5, 1.0); }\n";
        let corrected = Source::new(source).clip_depth();
        assert!(
            corrected.contains("gl_Position.z = (gl_Position.z + gl_Position.w) * 0.5;"),
            "{corrected}"
        );
        assert_eq!(
            corrected
                .matches("gl_Position.z = (gl_Position.z + gl_Position.w) * 0.5;")
                .count(),
            1,
            "the remap must be applied exactly once"
        );

        // The remap is exact at both ends of the GL volume: -w -> 0 and +w -> w.
        let map = |z: f32, w: f32| (z + w) * 0.5;
        assert_eq!(map(-1.0, 1.0), 0.0);
        assert_eq!(map(1.0, 1.0), 1.0);
        // The case the rung reports: z = -0.5 lands inside the host volume instead of outside it.
        assert!((0.0..=1.0).contains(&map(-0.5, 1.0)));
    }

    /// Depth and orientation are independent corrections and must compose without either being lost:
    /// an offscreen target takes the depth remap ALONE, a presented target takes both.
    #[test]
    fn depth_and_orientation_corrections_compose() {
        let source = "void main() { gl_Position = vec4(x, y, z, 1.0); }\n";
        let depth_corrected = Source::new(source).clip_depth();
        let both = Source::new(&depth_corrected).present_coordinates();
        assert!(both.contains("gl_Position.z = (gl_Position.z + gl_Position.w) * 0.5;"));
        assert!(both.contains("gl_Position.y = -gl_Position.y;"));

        let offscreen = Source::new(source).clip_depth();
        assert!(offscreen.contains("gl_Position.z = (gl_Position.z + gl_Position.w) * 0.5;"));
        assert!(
            !offscreen.contains("gl_Position.y = -gl_Position.y;"),
            "an offscreen target must not be flipped"
        );
    }

    #[test]
    fn present_target_reflects_vertex_y_once() {
        let source = "void main() { gl_Position = vec4(position.x, position.y, 0.0, 1.0); }\n";
        let corrected = Source::new(source).present_coordinates();
        assert!(corrected.contains(
            "gl_Position = vec4(position.x, position.y, 0.0, 1.0); \n gl_Position.y = -gl_Position.y;"
        ));
        assert_eq!(
            corrected.matches("gl_Position.y = -gl_Position.y;").count(),
            1
        );
    }

    #[test]
    fn rendered_texture_flips_y_but_never_x_for_es2_and_es3_sampling() {
        for function in ["texture", "texture2D"] {
            let source = format!(
                "void main() {{ vec2 uv = vec2(0.25, 0.75); color = {function}(atlas, uv); }}"
            );
            let rewritten =
                Source::new(&source).flip_render_target_samplers(&["atlas".to_string()]);
            assert!(
                rewritten.contains(&format!("{function}(atlas, vec2((uv).x, 1.0 - (uv).y))")),
                "{function} must map asymmetric (x,y)=(.25,.75) to (.25,.25), not (.75,.25)"
            );
        }
    }

    #[test]
    fn uploaded_texture_sampler_keeps_its_original_orientation() {
        let source = "void main() { color = texture2D(upload, uv) + texture2D(rendered, uv); }";
        let rewritten = Source::new(source).flip_render_target_samplers(&["rendered".to_string()]);
        assert!(rewritten.contains("texture2D(upload, uv)"));
        assert!(rewritten.contains("texture2D(rendered, vec2((uv).x, 1.0 - (uv).y))"));
    }

    #[test]
    fn overlapping_sampler_names_never_inherit_another_texture_orientation() {
        let source = "void main(){ color = texture(image, uv) + texture(images, uv); }";
        let rewritten = Source::new(source).transform_texture_samplers(&[
            ("image".to_string(), true, [0x1903, 0x1904, 0x1905, 0x1906]),
            (
                "images".to_string(),
                false,
                [0x1903, 0x1904, 0x1905, 0x1906],
            ),
        ]);
        assert!(rewritten.contains("texture(image, vec2((uv).x, 1.0 - (uv).y))"));
        assert!(rewritten.contains("texture(images, uv)"));
    }

    #[test]
    fn split_sampler_constructors_match_exact_scalar_and_array_bindings() {
        let source = "void main(){ color = texture(sampler2D(images_0_hltex, images_0_hlsmp), uv) \
                      + texture(sampler2D(image_hltex, image_hlsmp), uv); }";
        let rewritten = Source::new(source).flip_render_target_samplers(&["images[0]".to_string()]);
        assert!(rewritten.contains(
            "texture(sampler2D(images_0_hltex, images_0_hlsmp), vec2((uv).x, 1.0 - (uv).y))"
        ));
        assert!(rewritten.contains("texture(sampler2D(image_hltex, image_hlsmp), uv)"));
    }

    #[test]
    fn fragment_coordinates_restore_gl_origin_without_touching_upper_left() {
        let source = "void main() { color = vec4(gl_FragCoord.xy, 0.0, 1.0); }";
        let corrected = Source::new(source).fragment_coordinates(64, false, false);
        assert!(corrected.contains("vec4 hl_FragCoord;"));
        assert!(corrected.contains(
            "hl_FragCoord = vec4(gl_FragCoord.x, 64.0 - gl_FragCoord.y, gl_FragCoord.z, gl_FragCoord.w)"
        ));
        assert!(corrected.contains("vec4(hl_FragCoord.xy, 0.0, 1.0)"));
        assert_eq!(
            Source::new(source).fragment_coordinates(64, true, false),
            source
        );
    }

    #[test]
    fn integer_pixel_centers_shift_both_axes() {
        let corrected = Source::new("void main(){ color = gl_FragCoord; }")
            .fragment_coordinates(16, false, true);
        assert!(corrected.contains("gl_FragCoord.x - 0.5"));
        assert!(corrected.contains("16.0 - gl_FragCoord.y - 0.5"));
    }

    #[test]
    fn compute_uniform_blocks_use_a_namespace_after_ssbos() {
        let source = "#version 310 es
layout(std430, binding = 0) buffer Values { uint values[]; };
layout(std140, binding = 0) uniform Params { uint count; };
layout(std140) uniform More { uint offset; };
uniform uint plain;
void main() {}";
        let translated = Translator::compute(source);
        assert!(translated.contains("layout(std430, binding = 0) buffer Values"));
        assert!(translated.contains("layout(std140, binding = 8) uniform Params"));
        assert!(translated.contains("layout(std140, binding = 8) uniform More"));
        assert!(translated.contains("uniform uint plain"));
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL parsing / reflection helpers (shared by the desktop-GLSL emit above and the query/introspection
// reflection). Ported from hl-shim-gl/src/translate.rs.
// ---------------------------------------------------------------------------------------------------
