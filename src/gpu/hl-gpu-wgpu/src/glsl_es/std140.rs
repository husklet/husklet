use super::io::MatrixShape;
use super::*;

/// A parsed `layout(std140, …) uniform NAME { … } [instance];` interface block.
struct Std140Block {
    lb: usize, // index of the opening `{`
    rb: usize, // index of the matching `}`
    instance: Option<String>,
    end: usize, // index just past the terminating `;`
}

impl Std140Block {
    /// Parse a `layout(std140, …) uniform NAME { … } [instance];` block whose `layout` word is at `i`. Returns
    /// `None` for any `layout`/block that is not a `std140`-qualified uniform interface block.
    fn parse(toks: &[Tok], i: usize) -> Option<Self> {
        let lp = toks.next_significant(i + 1)?;
        if toks[lp] != Tok::Punct('(') {
            return None;
        }
        let rp = match_close(toks, lp, '(', ')');
        if rp >= toks.len() {
            return None;
        }
        let quals = toks[lp + 1..rp].source();
        // `std140` must appear as a whole qualifier word (not a substring of another identifier).
        let is_std140 = quals
            .split(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .any(|w| w == "std140");
        if !is_std140 {
            return None;
        }
        let u = toks.next_significant(rp + 1)?;
        if !matches!(&toks[u], Tok::Word(w) if w == "uniform") {
            return None;
        }
        let bn = toks.next_significant(u + 1)?; // block type name
        if !matches!(&toks[bn], Tok::Word(_)) {
            return None;
        }
        let lb = toks.next_significant(bn + 1)?;
        if toks[lb] != Tok::Punct('{') {
            return None;
        }
        let rb = match_close(toks, lb, '{', '}');
        if rb >= toks.len() {
            return None;
        }
        let after = toks.next_significant(rb + 1)?;
        let (instance, semi) = match &toks[after] {
            Tok::Word(name) => (Some(name.clone()), toks.next_significant(after + 1)?),
            Tok::Punct(';') => (None, after),
            _ => return None, // an array instance (`… x[2];`) or other shape — leave untouched
        };
        if toks[semi] != Tok::Punct(';') {
            return None;
        }
        Some(Self {
            lb,
            rb,
            instance,
            end: semi + 1,
        })
    }
}

/// Rewrite the body text of a std140 uniform block: each scalar `matNx2 NAME` member becomes `vec4
/// NAME__col[N]`, recording `(instance, NAME, cols)` in `members`. Non-2-row members are kept verbatim.
fn rewrite_std140_body(
    body: &str,
    instance: &str,
    members: &mut Vec<(String, String, u32)>,
) -> String {
    body.split(';')
        .map(|seg| match Mat2Member::parse(seg) {
            Some(member) => {
                members.push((instance.to_string(), member.name.clone(), member.columns));
                let lead: String = seg.chars().take_while(|c| c.is_whitespace()).collect();
                format!("{lead}vec4 {}__col[{}]", member.name, member.columns)
            }
            None => seg.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// If `seg` is a scalar 2-row-matrix member declaration (`mat2`/`mat3x2`/`mat4x2 NAME`, no array), return
/// `(NAME, cols)`; otherwise `None`. Precision qualifiers are already stripped before this pass runs.
struct Mat2Member {
    name: String,
    columns: u32,
}

impl Mat2Member {
    fn parse(seg: &str) -> Option<Self> {
        let toks = Tokens::from_source(seg);
        let sig: Vec<&Tok> = toks.iter().filter(|token| token.is_significant()).collect();
        if sig.iter().any(|t| matches!(t, Tok::Punct('['))) {
            return None; // an array member (`mat2 m[3]`) — not handled; leave for naga to reject
        }
        let base = match sig.first() {
            Some(Tok::Word(w)) => w.clone(),
            _ => return None,
        };
        let matrix = MatrixShape::parse(&base)?;
        if matrix.rows != 2 {
            return None;
        }
        let name = sig.iter().skip(1).find_map(|t| match t {
            Tok::Word(w) => Some(w.clone()),
            _ => None,
        })?;
        Some(Self {
            name,
            columns: matrix.columns,
        })
    }
}

/// The reconstructed matrix rvalue for a `block.member` use: `matN2(block.member__col[0].xy, …)`.
fn reconstruct_mat2(instance: &str, member: &str, cols: u32) -> String {
    let ctor = match cols {
        3 => "mat3x2",
        4 => "mat4x2",
        _ => "mat2",
    };
    let args: Vec<String> = (0..cols)
        .map(|k| format!("{instance}.{member}__col[{k}].xy"))
        .collect();
    format!("{ctor}({})", args.join(", "))
}

/// Rewrite every 2-row-matrix (`mat2`/`matNx2`) member of a `std140` uniform block to `vec4 col[N]` (same
/// std140 bytes) and reconstruct the matrix at every `block.member` use. See the module section header.
impl Tokens {
    pub(super) fn split_std140_mat2(&mut self) {
        let toks = &mut self.0;
        let mut members: Vec<(String, String, u32)> = Vec::new(); // (instance, member, cols)
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(b) = Std140Block::parse(toks, i) {
                    if let Some(instance) = b.instance.clone() {
                        let before = members.len();
                        let body = toks[b.lb + 1..b.rb].source();
                        let new_body = rewrite_std140_body(&body, &instance, &mut members);
                        if members.len() == before {
                            // No 2-row-matrix member — emit the block verbatim (byte-faithful).
                            out.extend(toks[i..b.end].iter().cloned());
                        } else {
                            out.extend(toks[i..=b.lb].iter().cloned()); // through the opening `{`
                            out.extend(Tokens::from_source(&new_body));
                            out.extend(toks[b.rb..b.end].iter().cloned()); // `}` … `;`
                        }
                        i = b.end;
                        continue;
                    }
                }
            }
            out.push(toks[i].clone());
            i += 1;
        }
        *toks = out;
        if members.is_empty() {
            return;
        }
        // Replace every `instance.member` use with the reconstructed matrix constructor.
        let mut result: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if let Tok::Word(w) = &toks[i] {
                if let Some(dot) = toks.next_significant(i + 1) {
                    if toks[dot] == Tok::Punct('.') {
                        if let Some(mem) = toks.next_significant(dot + 1) {
                            if let Tok::Word(m) = &toks[mem] {
                                if let Some((_, _, cols)) = members
                                    .iter()
                                    .find(|(inst, name, _)| inst == w && name == m)
                                {
                                    result.push(Tok::Word(reconstruct_mat2(w, m, *cols)));
                                    i = mem + 1;
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
            result.push(toks[i].clone());
            i += 1;
        }
        *toks = result;
    }
}

// ---------------------------------------------------------------------------------------------------
// Comment stripping (kept local so tokenization is self-contained)
// ---------------------------------------------------------------------------------------------------
