use super::*;

impl Tokens {
    pub(in crate::glsl_es) fn normalize_dual_source(&mut self) {
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
    pub(in crate::glsl_es) fn split_aggregate_io(&mut self, stage: naga::ShaderStage) {
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
pub(in crate::glsl_es) fn replace_ident(s: &str, from: &str, to: &str) -> String {
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
    pub(in crate::glsl_es) fn split_global_samplers(&mut self) -> Vec<(String, String)> {
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
    pub(in crate::glsl_es) fn split_param_samplers(&mut self) -> Vec<(String, String)> {
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
    pub(in crate::glsl_es) fn map_es_texture_builtins(&mut self) {
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
    pub(in crate::glsl_es) fn recombine_sampler_uses(
        &mut self,
        sampler_types: &[(String, String)],
    ) {
        let toks = &mut self.0;
        // Enclosing-call stack: each `(` pushes whether its call is a texture built-in (`Some(true)`), a user
        // call (`Some(false)`), or a grouping/keyword paren (`None`).
        let mut stack: Vec<Option<bool>> = Vec::new();
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut last_word: Option<String> = None;

        for token in toks.iter() {
            match token {
                Tok::Punct('(') => {
                    let ctx = match &last_word {
                        Some(word) if Identifier(word).is_keyword() => None,
                        Some(word) => Some(Identifier(word).is_texture_builtin()),
                        None => None,
                    };
                    stack.push(ctx);
                    out.push(token.clone());
                    last_word = None;
                }
                Tok::Punct(')') => {
                    stack.pop();
                    out.push(token.clone());
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
                    out.push(token.clone());
                }
                Tok::Ws(_) => out.push(token.clone()),
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
