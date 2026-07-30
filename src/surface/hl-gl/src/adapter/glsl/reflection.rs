use super::*;

impl StageSources<'_> {
    pub fn samplers(self) -> Vec<String> {
        let vs = Source::new(self.vertex).expanded();
        let fs = Source::new(self.fragment).expanded();
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
    pub(super) fn host_sampler(&self) -> Option<bool> {
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
        // Comment removal ONLY — deliberately not preprocessed. This replays the HOST's counter over the text
        // actually forwarded on the verbatim route, which the host preprocesses itself; expanding here would
        // drop the inactive external branch the host still counts and shift every `k`.
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
        let vs = Source::new(self.vertex).expanded();
        let fs = Source::new(self.fragment).expanded();
        let (data, _samps) = Declarations::from_stages(&vs, &fs).uniforms();
        data
    }

    /// The SAMPLER uniforms as `(name, glsl_type)` (`program_samplers` keeps only names; this keeps the
    /// declared sampler type so `glGetActiveUniform` can report `GL_SAMPLER_2D` vs `GL_SAMPLER_CUBE`).
    pub fn sampler_decls(self) -> Vec<Decl> {
        let vs = Source::new(self.vertex).expanded();
        let fs = Source::new(self.fragment).expanded();
        let (_data, samps) = Declarations::from_stages(&vs, &fs).uniforms();
        samps
    }

    /// The fragment-shader output variables a linked program declares (`out vecN name;`), in declaration
    /// order — the resource list `glGetFragDataLocation`/`glGetProgramResource*(GL_PROGRAM_OUTPUT)` resolve
    /// against. An ES2-style `gl_FragColor` shader declares none (its single output is location 0 implicitly).
    pub fn frag_outputs(self) -> Vec<Decl> {
        let fs = Source::new(self.fragment).expanded();
        let mut outs = Tokens(&fs).collect("out");
        outs.truncate(4);
        outs
    }
}
