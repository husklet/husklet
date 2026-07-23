use super::*;

pub(super) struct IoDecl {
    /// `true` for `in` (vertex attribute / fragment varying-in), `false` for `out` (vertex varying-out /
    /// fragment render-target).
    pub(super) is_in: bool,
    /// The declared identifier — the key that name-matches a vertex `out` to the fragment `in` varying.
    pub(super) name: String,
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
    pub(super) fn scan_io_decls(src: &str) -> Vec<IoDecl> {
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
    pub(super) fn parse_io_decl_forward(b: &[u8], q: usize) -> Option<String> {
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
    pub(super) fn preceding_io_qualifiers(
        b: &[u8],
        kw_start: usize,
    ) -> (usize, Option<usize>, Option<u32>) {
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
    pub(super) fn parse_location_value(grp: &[u8]) -> Option<u32> {
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
///
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
pub(super) struct Edits(Vec<(usize, String)>);

impl From<Vec<(usize, String)>> for Edits {
    fn from(edits: Vec<(usize, String)>) -> Self {
        Self(edits)
    }
}

impl Edits {
    pub(super) fn apply(mut self, src: &str) -> String {
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

// Sampler reflection continues in `reflection`.
