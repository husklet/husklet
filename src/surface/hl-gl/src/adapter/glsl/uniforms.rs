use super::*;

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
    pub(super) fn uniform_blocks(self) -> Vec<UniformBlockDecl> {
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
