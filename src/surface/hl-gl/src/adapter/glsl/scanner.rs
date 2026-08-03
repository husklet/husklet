use super::*;

impl Tokens<'_> {
    #[inline]
    pub(super) fn is_space(c: u8) -> bool {
        matches!(c, b' ' | b'\t' | b'\n' | b'\r')
    }
    #[inline]
    pub(super) fn is_word(c: u8) -> bool {
        c == b'_' || c.is_ascii_alphanumeric()
    }
    pub(super) fn is_precision_or_interp(t: &str) -> bool {
        matches!(
            t,
            "lowp" | "mediump" | "highp" | "flat" | "smooth" | "centroid"
        )
    }

    /// Brace/paren nesting depth of byte offset `at` within `b` (counting only the delimiters before it). A
    /// TOP-LEVEL interface declaration sits at `(0, 0)`; anything inside a function body (`{ … }`) or a
    /// parameter list (`( … )`) is nested. Used by [`collect`] to reject an `in`/`out`/`inout` FUNCTION
    /// PARAMETER (paren depth > 0) or a body-local declaration (brace depth > 0), which are not interface decls.
    pub(super) fn depth_at(b: &[u8], at: usize) -> (i32, i32) {
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
    pub(super) fn collect(&self, kw: &str) -> Vec<Decl> {
        let src = self.0;
        let constants = Constants::from_source(src);
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
                while *q < b.len() && !Self::is_space(b[*q]) && b[*q] != b';' {
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
            let on_type = Self::split_type_array(&mut ty, b, &mut q, &constants);
            while q < b.len() && Self::is_space(b[q]) {
                q += 1;
            }
            // std140 interface block: `uniform Block { TYPE m; ... } [inst];` — enumerate the MEMBERS.
            if q < b.len() && b[q] == b'{' {
                q += 1;
                loop {
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
                    let member_on_type = Self::split_type_array(&mut mty, b, &mut q, &constants);
                    while q < b.len() && Self::is_space(b[q]) {
                        q += 1;
                    }
                    let mut mnm = String::new();
                    while q < b.len() && Self::is_word(b[q]) {
                        mnm.push(b[q] as char);
                        q += 1;
                    }
                    let (marr, array_literal) = Self::merge_array_subscripts(
                        member_on_type,
                        Self::read_array_subscript(b, &mut q, &constants),
                    );
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
                            array_literal,
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
            // One GLSL declaration may contain several declarators:
            // `uniform float scale, bias[2], blend;`. The wrapper removes that whole statement, so every
            // name must be reflected and regenerated or later names become unknown variables.
            loop {
                while q < b.len() && Self::is_space(b[q]) {
                    q += 1;
                }
                let mut name = String::new();
                while q < b.len() && Self::is_word(b[q]) {
                    name.push(b[q] as char);
                    q += 1;
                }
                let (arr, array_literal) = Self::merge_array_subscripts(
                    on_type,
                    Self::read_array_subscript(b, &mut q, &constants),
                );
                if !ty.is_empty() && !name.is_empty() {
                    out.push(Decl {
                        ty: ty.clone(),
                        name,
                        arr,
                        array_literal,
                    });
                }
                while q < b.len() && Self::is_space(b[q]) {
                    q += 1;
                }
                if q >= b.len() || b[q] != b',' {
                    break;
                }
                q += 1;
            }
            p = q;
        }
        out
    }

    /// GLSL puts the array size on the TYPE as readily as on the NAME: `uniform vec4[3] x;` is exactly
    /// `uniform vec4 x[3];` (GLSL ES 3.00 §4.1.9, and desktop GLSL since 1.20). `read_tok` stops only at a
    /// space or `;`, so it swallows the subscript into the type token and every type table then rejects
    /// `vec4[3]` as an unknown type — which is how a legal GskGLRenderer shader failed to link.
    ///
    /// Split any subscript off `ty` (both the joined `vec4[3]` and the detached `vec4 [3] name` spelling)
    /// and return it in [`read_array_subscript`]'s `(elements, modelable)` form; `(0, true)` when the type
    /// carries none.
    pub(super) fn split_type_array(
        ty: &mut String,
        b: &[u8],
        q: &mut usize,
        constants: &Constants,
    ) -> (u32, bool) {
        if let Some(open) = ty.find('[') {
            let subscript = ty.split_off(open);
            let bytes = subscript.as_bytes();
            let mut at = 0usize;
            let (size, modelable) = Self::read_array_subscript(bytes, &mut at, constants);
            // Anything trailing the `]` (`vec4[2][3]`, or a name jammed against the type) is not the single
            // dimension this uniform model can lay out; refuse rather than silently keep the first size.
            return (size, modelable && at == bytes.len());
        }
        // `TYPE [n] name` — the subscript stands alone between the type and the declarator.
        Self::read_array_subscript(b, q, constants)
    }

    /// Combine a type-side and a name-side array subscript for one declarator. Exactly one may be present:
    /// GLSL ES has no arrays of arrays, so `vec4[3] x[2]` is refused (`modelable = false`) rather than
    /// collapsing to whichever dimension we happened to read last.
    pub(super) fn merge_array_subscripts(
        on_type: (u32, bool),
        on_name: (u32, bool),
    ) -> (u32, bool) {
        match (on_type, on_name) {
            ((0, true), name) => name,
            (ty, (0, true)) => ty,
            _ => (0, false),
        }
    }

    /// If the bytes at `*q` are an array subscript `[dimension]` (possibly with surrounding spaces), consume
    /// it and return the element count; otherwise leave `*q` unchanged and return `(0, true)` (not an array).
    ///
    /// GLSL ES 1.00 §4.1.9 requires the dimension to be an INTEGRAL CONSTANT EXPRESSION, not merely a
    /// literal: after preprocessing that admits `#define`d sizes (already substituted), arithmetic on
    /// literals, and the stage's global `const int` values, all of which [`Constants`] folds. A dimension
    /// that is not constant returns `(0, false)`, which link validation rejects explicitly.
    pub(super) fn read_array_subscript(
        b: &[u8],
        q: &mut usize,
        constants: &Constants,
    ) -> (u32, bool) {
        let mut p = *q;
        while p < b.len() && Self::is_space(b[p]) {
            p += 1;
        }
        if p >= b.len() || b[p] != b'[' {
            return (0, true);
        }
        p += 1;
        let start = p;
        let mut depth = 0usize;
        while p < b.len() {
            match b[p] {
                b'[' | b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b']' if depth == 0 => break,
                b']' => depth -= 1,
                b';' | b'{' | b'}' => break,
                _ => {}
            }
            p += 1;
        }
        if p >= b.len() || b[p] != b']' {
            *q = p;
            return (0, false);
        }
        let dimension = std::str::from_utf8(&b[start..p]).unwrap_or("");
        match constants.dimension(dimension) {
            Some(size) => {
                *q = p + 1;
                (size, true)
            }
            None => {
                *q = p + 1;
                (0, false)
            }
        }
    }
}

pub(super) fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
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
///
/// `None` means the body could not be found: there is no `main(` at all, or its opening brace is never
/// closed. That is DISTINCT from `Some("")`, which is the legitimately empty body of `void main(){}`.
/// Both used to return the empty string, and the consequence was not a wrong error but a wrong RENDER —
/// the translator regenerated `void main() {}` from a shader with a dropped brace, the host front end
/// correctly accepted it, the draw executed and wrote nothing, and no layer reported anything.
impl Source<'_> {
    /// Whether this stage declares the built-in position output invariant. The translator regenerates
    /// top-level declarations, so this declaration must be carried explicitly rather than left behind with
    /// the original source text.
    pub(super) fn has_invariant_position(self) -> bool {
        let bytes = self.text.as_bytes();
        let mut cursor = 0usize;
        while let Some(relative) = self.text[cursor..].find("invariant") {
            let start = cursor + relative;
            cursor = start + "invariant".len();
            if start > 0 && Tokens::is_word(bytes[start - 1]) {
                continue;
            }
            let mut name = cursor;
            while bytes.get(name).is_some_and(|byte| Tokens::is_space(*byte)) {
                name += 1;
            }
            if self.text[name..].starts_with("gl_Position") {
                let end = name + "gl_Position".len();
                if !bytes.get(end).is_some_and(|byte| Tokens::is_word(*byte)) {
                    return true;
                }
            }
        }
        false
    }

    pub(super) fn main_body(self) -> Option<String> {
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
        let p = open?;
        let mut depth = 0i32;
        let mut k = p;
        while k < n {
            match b[k] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(String::from_utf8_lossy(&b[p + 1..k]).into_owned());
                    }
                }
                _ => {}
            }
            k += 1;
        }
        // The opening brace is never closed: the source is truncated or unbalanced.
        None
    }

    /// Whether this stage has a `main` whose body can be found and is closed.
    ///
    /// The predicate `Program::link` refuses on. A stage that fails it is not translatable, and the
    /// translator's regeneration would otherwise turn it into a shader that compiles and draws nothing.
    pub fn has_main_body(self) -> bool {
        self.main_body().is_some()
    }
}

/// Word-boundary replace `from`→`to` (gl_shim.c `wreplace`).
pub(super) fn wreplace(buf: &mut String, from: &str, to: &str) {
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
pub(super) fn sreplace(buf: &mut String, from: &str, to: &str) {
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
    pub(super) fn consts(self) -> Vec<String> {
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

/// Strip every preprocessor line (`#version`/`#define`/`#ifdef`/…). `translate_render` pins its own desktop
/// `#version` and reflects the interface directly, so ES preprocessor directives are dropped on this path —
/// the macro-heavy GskGpu/ANGLE shaders that actually depend on them take the VERBATIM host route (whose
/// `glsl_es::normalize` runs naga's real preprocessor) instead of the reflect-and-regenerate path.
impl Source<'_> {
    pub(super) fn without_preprocessor(self) -> String {
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
    pub(super) fn top_level_units(self) -> Vec<String> {
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

// Comment removal lives with the preprocessing phase it feeds (`preprocess::comment`); token-level
// declaration parsing continues in `tokens`.
