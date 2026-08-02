use super::preprocessor::Preprocessor;
use super::*;

pub(super) struct SwitchGroup {
    values: Vec<String>,
    is_default: bool,
    body: String,
}

/// A lowered switch and the token index just past its closing `}`.
pub(super) struct SwitchRewrite {
    text: String,
    end: usize,
}

/// Index of the `Punct(close)` matching the `Punct(open)` at `open_idx`, counting nesting; `toks.len()` if
/// unbalanced. Only `Tok::Punct` participates — punctuation embedded inside a merged `Tok::Word` (e.g. the
/// `sampler2D(a, b)` recombination) is opaque and always balanced, so it never disturbs the count.
pub(super) fn match_close(toks: &[Tok], open_idx: usize, open: char, close: char) -> usize {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().skip(open_idx) {
        if let Tok::Punct(c) = t {
            if *c == open {
                depth += 1;
            } else if *c == close {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
        }
    }
    toks.len()
}

/// Index of the first top-level `:` (paren/brace depth 0) at or after `start`, ending a `case`/`default`
/// label; `None` if absent.
impl SwitchGroup {
    pub(super) fn top_colon(toks: &[Tok], start: usize) -> Option<usize> {
        let (mut p, mut b) = (0i32, 0i32);
        for (i, t) in toks.iter().enumerate().skip(start) {
            match t {
                Tok::Punct('(') => p += 1,
                Tok::Punct(')') => p -= 1,
                Tok::Punct('{') => b += 1,
                Tok::Punct('}') => b -= 1,
                Tok::Punct(':') if p == 0 && b == 0 => return Some(i),
                _ => {}
            }
        }
        None
    }

    /// Split a switch body's tokens into `case`/`default` groups, merging stacked (empty-body) labels into the
    /// following group. Returns `None` only if a label is missing its `:`.
    pub(super) fn parse(body: &[Tok]) -> Option<Vec<Self>> {
        let mut groups: Vec<Self> = Vec::new();
        let mut cur_values: Vec<String> = Vec::new();
        let mut cur_default = false;
        let mut cur_body: Vec<Tok> = Vec::new();
        let (mut paren, mut brace) = (0i32, 0i32);
        let mut i = 0;
        while i < body.len() {
            let at_top = paren == 0 && brace == 0;
            if at_top {
                if let Tok::Word(w) = &body[i] {
                    if w == "case" || w == "default" {
                        if cur_body.iter().any(Tok::is_significant) {
                            groups.push(Self {
                                values: std::mem::take(&mut cur_values),
                                is_default: std::mem::take(&mut cur_default),
                                body: Self::body_without_break(&cur_body),
                            });
                            cur_body.clear();
                        }
                        let colon = Self::top_colon(body, i + 1)?;
                        if w == "default" {
                            cur_default = true;
                        } else {
                            cur_values.push(body[i + 1..colon].source());
                        }
                        i = colon + 1;
                        continue;
                    }
                }
            }
            if let Tok::Punct(c) = &body[i] {
                match c {
                    '(' => paren += 1,
                    ')' => paren -= 1,
                    '{' => brace += 1,
                    '}' => brace -= 1,
                    _ => {}
                }
            }
            cur_body.push(body[i].clone());
            i += 1;
        }
        if cur_default || !cur_values.is_empty() || cur_body.iter().any(Tok::is_significant) {
            groups.push(Self {
                values: cur_values,
                is_default: cur_default,
                body: Self::body_without_break(&cur_body),
            });
        }
        Some(groups)
    }

    pub(super) fn body_without_break(body: &[Tok]) -> String {
        let sig: Vec<usize> = (0..body.len())
            .filter(|&j| body[j].is_significant())
            .collect();
        if sig.len() >= 2 {
            let last = sig[sig.len() - 1];
            let prev = sig[sig.len() - 2];
            if matches!(&body[last], Tok::Punct(';'))
                && matches!(&body[prev], Tok::Word(w) if w == "break")
            {
                let kept: Vec<Tok> = body
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != last && *j != prev)
                    .map(|(_, t)| t.clone())
                    .collect();
                return kept.as_slice().source();
            }
        }
        body.source()
    }
}

/// Lower every GLSL `switch` statement to an equivalent `if / else if / else` chain.
///
/// naga's `glsl-in` marks a `switch` case as `fall_through` unless it ends in a *literal* `break`: its
/// case-terminator is recorded with `get_or_insert` (`front/glsl/parser/functions.rs`), so a case whose
/// first terminator is a `return` — every GskGpu color-state / slice `switch` — stays fall-through, and
/// `wgsl-out` then rejects the non-empty fall-through case (`back/wgsl/writer.rs`). GskGpu never falls
/// through a *non-empty* case (each ends in `return`), so an if/else chain is semantically identical and
/// sidesteps naga's switch modeling entirely. The selector is a side-effect-free expression (`cs`,
/// `slice`, …), so it is repeated in each equality test rather than spilled to a temporary whose type we
/// would have to infer. Stacked (empty) labels OR into the following case's condition; `default` (wherever
/// it appears) becomes the trailing `else`. Nested switches are lowered first.
impl SwitchRewrite {
    pub(super) fn lower_all(toks: &[Tok]) -> Vec<Tok> {
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "switch") {
                if let Some(rep) = Self::from_tokens(toks, i) {
                    out.extend(Tokens::from_source(&rep.text));
                    i = rep.end;
                    continue;
                }
            }
            out.push(toks[i].clone());
            i += 1;
        }
        out
    }

    /// Attempt to lower the `switch` whose keyword is at `switch_idx`. Returns `None` (leaving the switch
    /// untouched) if the shape is not the expected `switch (SELECTOR) { … }`.
    pub(super) fn from_tokens(toks: &[Tok], switch_idx: usize) -> Option<Self> {
        let lp = toks.next_significant(switch_idx + 1)?;
        if toks[lp] != Tok::Punct('(') {
            return None;
        }
        let rp = match_close(toks, lp, '(', ')');
        if rp >= toks.len() {
            return None;
        }
        let lb = toks.next_significant(rp + 1)?;
        if toks[lb] != Tok::Punct('{') {
            return None;
        }
        let rb = match_close(toks, lb, '{', '}');
        if rb >= toks.len() {
            return None;
        }

        let selector = toks[lp + 1..rp].source();
        let selector = selector.trim();
        // Lower any nested switches in the body before parsing this one, so inner `case`s are gone.
        let body = Self::lower_all(&toks[lb + 1..rb]);
        let groups = SwitchGroup::parse(&body)?;

        let mut nondefault: Vec<&SwitchGroup> = Vec::new();
        let mut default_body: Option<&str> = None;
        for g in &groups {
            if g.body.trim().is_empty() && !g.is_default {
                continue; // a degenerate empty non-default group matches nothing meaningful
            }
            if g.is_default {
                default_body = Some(g.body.as_str());
            } else {
                nondefault.push(g);
            }
        }

        let mut text = String::from("{\n");
        for (k, g) in nondefault.iter().enumerate() {
            let cond = g
                .values
                .iter()
                .map(|v| format!("(({selector}) == ({}))", v.trim()))
                .collect::<Vec<_>>()
                .join(" || ");
            let kw = if k == 0 { "if" } else { "else if" };
            text.push_str(&format!("{kw} ({cond}) {{\n{}\n}}\n", g.body));
        }
        if let Some(db) = default_body {
            if nondefault.is_empty() {
                text.push_str(&format!("{{\n{db}\n}}\n"));
            } else {
                text.push_str(&format!("else {{\n{db}\n}}\n"));
            }
        }
        text.push_str("}\n");
        Some(Self { text, end: rb + 1 })
    }
}

/// `#version … [es]` → `#version 460`; `gl_VertexID`/`gl_InstanceID` → the naga builtins; drop `precision
/// …;` statements and inline `highp`/`mediump`/`lowp` qualifiers.
impl Tokens {
    pub(super) fn normalize_directives_and_precision(&mut self) {
        let toks = &mut self.0;
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            match &toks[i] {
                Tok::Pp(p) if p.trim_start().starts_with("#version") => {
                    out.push(Tok::Pp(DESKTOP_VERSION.trim_end().to_string()));
                }
                // Preprocessor lines are opaque tokens, but GskGpu hides the vertex-index builtin inside a
                // `#define GSK_VERTEX_INDEX gl_VertexID` macro *body*: naga's preprocessor expands that macro at
                // every use site, so the raw `gl_VertexID` would reach the parser and be rejected. Rewrite the
                // builtins textually inside the directive so the expanded token is already the naga builtin.
                Tok::Pp(p) => {
                    let rewritten = Preprocessor(p)
                        .io_macro()
                        .unwrap_or_else(|| Preprocessor(p).vertex_builtins());
                    out.push(Tok::Pp(rewritten));
                }
                // naga's `gl_VertexIndex`/`gl_InstanceIndex` builtins are `uint`, but the GLES `gl_VertexID`/
                // `gl_InstanceID` they replace are `int`; wrap in an `int(…)` cast so a `int id = gl_VertexID;`
                // assignment keeps matching store types (naga validates this).
                Tok::Word(w) if w == "gl_VertexID" => {
                    out.push(Tok::Word("int(gl_VertexIndex)".into()))
                }
                Tok::Word(w) if w == "gl_InstanceID" => {
                    out.push(Tok::Word("int(gl_InstanceIndex)".into()))
                }
                Tok::Word(w) if w == "highp" || w == "mediump" || w == "lowp" => {
                    // Drop the qualifier; also drop a following separating space so we don't leave a double gap.
                    if matches!(toks.get(i + 1), Some(Tok::Ws(_))) {
                        i += 1;
                    }
                }
                Tok::Word(w) if w == "precision" => {
                    // `precision <qual> <type> ;` — a whole statement with no runtime effect in core GLSL.
                    while i < toks.len() && toks[i] != Tok::Punct(';') {
                        i += 1;
                    }
                    // skip the ';' too (loop's i += 1 below advances past it)
                }
                other => out.push(other.clone()),
            }
            i += 1;
        }
        *toks = out;
    }

    /// WGSL cannot represent GLSL's `PointSize` builtin. The GL service advertises the only point-size range
    /// wgpu can render, `[1, 1]`, so redirect the vertex-stage variable to ordinary private storage. This keeps
    /// expression evaluation and shader-local reads intact while the rasterizer supplies the contracted unity
    /// size. A declaration is inserted after directives because GLSL requires `#version` to remain first.
    pub(super) fn normalize_fixed_point_size(&mut self, stage: naga::ShaderStage) {
        if stage != naga::ShaderStage::Vertex
            || !self
                .0
                .iter()
                .any(|token| matches!(token, Tok::Word(word) if word == "gl_PointSize"))
        {
            return;
        }

        let mut name = "hl_point_size".to_string();
        while self
            .0
            .iter()
            .any(|token| matches!(token, Tok::Word(word) if word == &name))
        {
            name.push('_');
        }
        for token in &mut self.0 {
            if matches!(token, Tok::Word(word) if word == "gl_PointSize") {
                *token = Tok::Word(name.clone());
            }
        }

        let declaration = Tokens::from_source(&format!("float {name} = 1.0;\n")).0;
        let insertion = self
            .0
            .iter()
            .position(|token| !matches!(token, Tok::Pp(_) | Tok::Ws(_)))
            .unwrap_or(self.0.len());
        self.0.splice(insertion..insertion, declaration);
    }
}
