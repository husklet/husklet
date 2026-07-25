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
    pub(super) fn read_array_subscript(b: &[u8], q: &mut usize) -> u32 {
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
impl Source<'_> {
    pub(super) fn main_body(self) -> String {
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

/// Strip `//` and `/* */` comments (gl_shim.c `strip_comments`).
impl Source<'_> {
    pub(super) fn comments_removed(self) -> String {
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

// Token-level declaration parsing continues in `tokens`.
