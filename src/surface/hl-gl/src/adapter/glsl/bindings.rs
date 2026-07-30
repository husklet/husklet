use super::*;

pub(super) struct UniformBlockEdits<'a> {
    src: &'a str,
}

impl<'a> UniformBlockEdits<'a> {
    pub(super) fn new(src: &'a str) -> Self {
        Self { src }
    }

    pub(super) fn apply(self) -> String {
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
    pub(super) fn preceding_layout_binding(b: &[u8], uniform_pos: usize) -> (Option<usize>, bool) {
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
/// and the block is emitted in the unconditional directive prologue. It must never inherit a declaration's
/// `#if`: the first textual uniform can be in an inactive ANGLE branch while a later active branch uses a
/// different member. A sampler global or a block member is never touched. Returns the source unchanged when
/// the stage has no bare depth-0 data uniform.
impl Source<'_> {
    pub(super) fn wrap_default_block_uniforms(self, combined: &[Decl]) -> String {
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
        // Emit the combined std140 block once in the unconditional prologue. Splicing it at the first removed
        // declaration is unsound: that declaration may sit below `#if 0`, which preprocesses the replacement
        // block away after every later bare declaration has already been removed.
        let mut block = String::new();
        Declarations::emit_uniform_block(&mut block, combined);
        let insert_at = self.uniform_block_insertion();
        debug_assert!(insert_at <= removals[0].0);
        let mut out = String::with_capacity(n + block.len());
        out.push_str(&src[..insert_at]);
        out.push_str(&block);
        let mut cursor = insert_at;
        for (s, e) in &removals {
            out.push_str(&src[cursor..*s]);
            cursor = *e;
        }
        out.push_str(&src[cursor..]);
        out
    }

    /// Byte position after the stage's leading unconditional directive prologue. Conditional directives end
    /// the prologue: inserting after `#if` would make the generated block conditional. Comments, blank lines,
    /// `#version`, extensions, defines, pragmas, and continued directive lines remain before the block.
    fn uniform_block_insertion(self) -> usize {
        let bytes = self.text.as_bytes();
        let mut offset = 0usize;
        let mut block_comment = false;
        let mut directive_continuation = false;

        while offset < bytes.len() {
            let line_end = bytes[offset..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |relative| offset + relative + 1);
            let mut line = &self.text[offset..line_end];

            if directive_continuation {
                directive_continuation = line.trim_end().ends_with('\\');
                offset = line_end;
                continue;
            }

            loop {
                line = line.trim_start();
                if block_comment {
                    let Some(end) = line.find("*/") else {
                        offset = line_end;
                        break;
                    };
                    line = &line[end + 2..];
                    block_comment = false;
                    continue;
                }
                if line.starts_with("/*") {
                    block_comment = true;
                    line = &line[2..];
                    continue;
                }
                break;
            }
            if block_comment {
                continue;
            }

            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with("//") {
                offset = line_end;
                continue;
            }
            let Some(directive) = trimmed.strip_prefix('#') else {
                break;
            };
            let name = directive
                .trim_start()
                .split(|character: char| character.is_whitespace() || character == '(')
                .next()
                .unwrap_or("");
            if matches!(name, "if" | "ifdef" | "ifndef" | "elif" | "else" | "endif") {
                break;
            }
            directive_continuation = trimmed.trim_end().ends_with('\\');
            offset = line_end;
        }

        offset
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
    prepare_verbatim_program_with(vs, fs, combined, &std::collections::BTreeMap::new())
}

pub fn prepare_verbatim_program_with(
    vs: &str,
    fs: &str,
    combined: &[Decl],
    attribute_bindings: &std::collections::BTreeMap<String, u32>,
) -> (String, String) {
    let vs_u = Source::new(vs).prepare_verbatim_stage(combined);
    let fs_u = Source::new(fs).prepare_verbatim_stage(combined);
    StageSources::new(&vs_u, &fs_u).inject_io_locations_with(attribute_bindings)
}

// A depth-0 `in`/`out` interface declaration found in a verbatim stage (an attribute, a varying, or a
// fragment output) is modeled in `locations`.
