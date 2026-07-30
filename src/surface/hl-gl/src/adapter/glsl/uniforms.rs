use super::*;

impl StageSources<'_> {
    pub fn uniform_layout(self) -> Result<(Vec<Uni>, i32), UniformError> {
        let vs = Source::new(self.vertex).preprocessed()?;
        let fs = Source::new(self.fragment).preprocessed()?;
        Self::validate_declaration_compatibility(&vs, &fs)?;
        Self::validate_stage("vertex", &vs)?;
        Self::validate_stage("fragment", &fs)?;
        let vertex_data = Declarations::from_stages(&vs, "").uniforms().0;
        let fragment_data = Declarations::from_stages(&fs, "").uniforms().0;
        for vertex in &vertex_data {
            if let Some(fragment) = fragment_data
                .iter()
                .find(|fragment| fragment.name == vertex.name)
            {
                if fragment.ty != vertex.ty || fragment.arr != vertex.arr {
                    return Err(UniformError::ConflictingDeclaration(vertex.name.clone()));
                }
            }
        }
        let vertex_samplers = Declarations::from_stages(&vs, "").uniforms().1;
        let fragment_samplers = Declarations::from_stages(&fs, "").uniforms().1;
        for vertex in &vertex_samplers {
            if let Some(fragment) = fragment_samplers
                .iter()
                .find(|fragment| fragment.name == vertex.name)
            {
                if fragment.ty != vertex.ty || fragment.arr != vertex.arr {
                    return Err(UniformError::ConflictingDeclaration(vertex.name.clone()));
                }
            }
        }
        let (unis, samplers) = Declarations::from_stages(&vs, &fs).uniforms();
        let samplers = sampler_elements(&samplers)?;
        if samplers > MAX_COMBINED_SAMPLERS {
            return Err(UniformError::Samplers(samplers));
        }
        let mut cur = 0usize;
        let mut out = Vec::new();
        for d in &unis {
            if !d.array_literal {
                return Err(UniformError::NonLiteralArray(d.name.clone()));
            }
            let (esz, eal, _) = TypeToken(&d.ty)
                .layout()
                .ok_or_else(|| UniformError::UnsupportedType(d.ty.clone()))?;
            // std140: an ARRAY member rounds each element's stride UP to a vec4 (16 B) and aligns the member to
            // 16 B; a scalar/vector/matrix member keeps its natural size/alignment.
            let (sz, al) = if d.arr > 0 {
                let stride = esz
                    .checked_add(15)
                    .ok_or(UniformError::ArithmeticOverflow)?
                    & !15;
                (
                    stride
                        .checked_mul(d.arr as usize)
                        .ok_or(UniformError::ArithmeticOverflow)?,
                    eal.max(16),
                )
            } else {
                (esz, eal)
            };
            cur = cur
                .checked_add(al - 1)
                .ok_or(UniformError::ArithmeticOverflow)?
                & !(al - 1);
            out.push(Uni {
                name: d.name.clone(),
                off: i32::try_from(cur).map_err(|_| UniformError::ArithmeticOverflow)?,
                sz: i32::try_from(sz).map_err(|_| UniformError::ArithmeticOverflow)?,
                ty: d.ty.clone(),
                arr: d.arr,
            });
            cur = cur
                .checked_add(sz)
                .ok_or(UniformError::ArithmeticOverflow)?;
        }
        let total = cur
            .checked_add(15)
            .ok_or(UniformError::ArithmeticOverflow)?
            & !15;
        if total > MAX_COMBINED_UNIFORM_BYTES {
            return Err(UniformError::BlockBytes(total));
        }
        Ok((
            out,
            i32::try_from(total).map_err(|_| UniformError::ArithmeticOverflow)?,
        ))
    }

    fn validate_declaration_compatibility(
        vertex: &str,
        fragment: &str,
    ) -> Result<(), UniformError> {
        let declarations = Tokens(vertex)
            .collect("uniform")
            .into_iter()
            .chain(Tokens(fragment).collect("uniform"))
            .filter(|declaration| declaration.ty != "samplerExternalOES")
            .collect::<Vec<_>>();
        for (index, declaration) in declarations.iter().enumerate() {
            if declarations[index + 1..].iter().any(|other| {
                other.name == declaration.name
                    && (other.ty != declaration.ty || other.arr != declaration.arr)
            }) {
                return Err(UniformError::ConflictingDeclaration(
                    declaration.name.clone(),
                ));
            }
        }
        Ok(())
    }

    fn validate_stage(stage: &'static str, source: &str) -> Result<(), UniformError> {
        let (data, samplers) = Declarations::from_stages(source, "").uniforms();
        let samplers = sampler_elements(&samplers)?;
        let sampler_limit = if stage == "vertex" {
            MAX_VERTEX_SAMPLERS
        } else {
            MAX_FRAGMENT_SAMPLERS
        };
        if samplers > sampler_limit {
            return Err(UniformError::Samplers(samplers));
        }
        let mut components = 0usize;
        for declaration in data {
            if !declaration.array_literal {
                return Err(UniformError::NonLiteralArray(declaration.name));
            }
            let (_, _, element_components) = TypeToken(&declaration.ty)
                .layout()
                .ok_or_else(|| UniformError::UnsupportedType(declaration.ty.clone()))?;
            let elements = if declaration.arr == 0 {
                1
            } else {
                declaration.arr as usize
            };
            components = components
                .checked_add(
                    element_components
                        .checked_mul(elements)
                        .ok_or(UniformError::ArithmeticOverflow)?,
                )
                .ok_or(UniformError::ArithmeticOverflow)?;
        }
        if components > MAX_UNIFORM_COMPONENTS {
            return Err(UniformError::StageComponents {
                stage,
                count: components,
            });
        }
        Ok(())
    }

    pub fn validate_sampler_array_uses(self) -> Result<(), UniformError> {
        // The first gate `Program::link` runs, and therefore where a preprocessor diagnostic must surface:
        // no unexpanded macro may reach the reflection below or the host compiler beyond it.
        let vertex = Source::new(self.vertex).preprocessed()?;
        let fragment = Source::new(self.fragment).preprocessed()?;
        let declarations = Declarations::from_stages(&vertex, &fragment).uniforms().1;
        for declaration in declarations
            .iter()
            .filter(|declaration| declaration.arr > 1 && declaration.array_literal)
        {
            for source in [&vertex, &fragment] {
                let mut cursor = 0usize;
                let needle = format!("{}[", declaration.name);
                while let Some(relative) = source[cursor..].find(&needle) {
                    let open = cursor + relative + declaration.name.len();
                    let Some(close_relative) = source[open + 1..].find(']') else {
                        return Err(UniformError::DynamicSamplerArray(declaration.name.clone()));
                    };
                    let close = open + 1 + close_relative;
                    let index = source[open + 1..close].trim();
                    if index.parse::<u32>().is_err() {
                        return Err(UniformError::DynamicSamplerArray(declaration.name.clone()));
                    }
                    cursor = close + 1;
                }
            }
        }
        Ok(())
    }
}

fn sampler_elements(declarations: &[Decl]) -> Result<usize, UniformError> {
    declarations.iter().try_fold(0usize, |total, declaration| {
        if !declaration.array_literal {
            return Err(UniformError::NonLiteralArray(declaration.name.clone()));
        }
        total
            .checked_add(declaration.arr.max(1) as usize)
            .ok_or(UniformError::ArithmeticOverflow)
    })
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
            let src = Source::new(src).expanded();
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
    pub(super) fn uniform_blocks(self) -> Vec<UniformBlockDecl> {
        let src = self.text;
        let constants = Constants::from_source(src);
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
            while q < b.len() && b[q] != b'}' {
                while q < b.len() && (Tokens::is_space(b[q]) || b[q] == b';') {
                    q += 1;
                }
                if q >= b.len() || b[q] == b'}' {
                    break;
                }
                let read_tok = |q: &mut usize| -> String {
                    let mut s = String::new();
                    while *q < b.len() && !Tokens::is_space(b[*q]) && b[*q] != b';' && b[*q] != b'}'
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
                let (arr, array_literal) = Tokens::read_array_subscript(b, &mut q, &constants);
                if !ty.is_empty() && !name.is_empty() {
                    members.push(Decl {
                        ty,
                        name,
                        arr,
                        array_literal,
                    });
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
        let src = Source::new(self.text).expanded();
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

// Binding injection continues in `bindings`.
/* Inject `layout(binding = N)` into every uniform BLOCK that LACKS an explicit binding, before a stage is
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
so injecting here would double-qualify them. */
