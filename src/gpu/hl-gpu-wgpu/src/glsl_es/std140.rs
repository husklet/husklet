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
    instance: Option<&str>,
    skip: &[String],
    members: &mut Vec<Member>,
) -> String {
    body.split(';')
        .map(|seg| match Mat2Member::parse(seg) {
            Some(member) if !skip.contains(&member.name) => {
                members.push(match instance {
                    Some(instance) => Member::Qualified {
                        instance: instance.to_string(),
                        name: member.name.clone(),
                        columns: member.columns,
                    },
                    None => Member::Bare {
                        name: member.name.clone(),
                        columns: member.columns,
                    },
                });
                let lead: String = seg.chars().take_while(|c| c.is_whitespace()).collect();
                format!("{lead}vec4 {}__col[{}]", member.name, member.columns)
            }
            _ => seg.to_string(),
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

/// The reconstructed matrix rvalue for a use of a rewritten member: `matN2(P__col[0].xy, …)`, where `P`
/// is `block.member` for a named-instance block and the bare `member` for an anonymous one (whose members
/// live in global scope).
fn reconstruct_mat2(path: &str, cols: u32) -> String {
    let ctor = match cols {
        3 => "mat3x2",
        4 => "mat4x2",
        _ => "mat2",
    };
    let args: Vec<String> = (0..cols)
        .map(|k| format!("{path}__col[{k}].xy"))
        .collect();
    format!("{ctor}({})", args.join(", "))
}

/// A member the block pass rewrote, and how its uses are spelled.
///
/// `Qualified` is a named-instance block (`… } x;`): uses read `x.m`, matched as a `Word '.' Word` pair,
/// which cannot collide with anything else because the instance name qualifies it.
///
/// `Bare` is an ANONYMOUS block (`… };`), whose members GLSL places in GLOBAL SCOPE, so uses read `m` with
/// nothing to qualify them. A bare identifier can be shadowed by a local of the same name, and this pass
/// has no scope tracking, so a bare member is rewritten only when [`Tokens::shadowed_bare_members`] finds
/// no competing declaration of that name. When it is ambiguous the member is left alone: naga then rejects
/// the shader loudly, which is far better than silently reading the wrong value.
enum Member {
    Qualified { instance: String, name: String, columns: u32 },
    Bare { name: String, columns: u32 },
}

impl Member {
    fn columns(&self) -> u32 {
        match self {
            Self::Qualified { columns, .. } | Self::Bare { columns, .. } => *columns,
        }
    }
}

/// Rewrite every 2-row-matrix (`mat2`/`matNx2`) member of a `std140` uniform block to `vec4 col[N]` (same
/// std140 bytes) and reconstruct the matrix at every `block.member` use. See the module section header.
impl Tokens {
    pub(super) fn split_std140_mat2(&mut self) {
        let toks = &mut self.0;
        // An anonymous block's members land in GLOBAL scope, so their uses are bare identifiers that a
        // local could shadow. Decide which bare names are safe to touch BEFORE rewriting anything, against
        // the untouched token stream — an unsafe name is then left declared and used exactly as it was.
        let unsafe_bare = Self::shadowed_bare_members(toks);

        let mut members: Vec<Member> = Vec::new();
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(b) = Std140Block::parse(toks, i) {
                    let before = members.len();
                    let body = toks[b.lb + 1..b.rb].source();
                    let skip: &[String] = if b.instance.is_none() { &unsafe_bare } else { &[] };
                    let new_body =
                        rewrite_std140_body(&body, b.instance.as_deref(), skip, &mut members);
                    if members.len() == before {
                        // No rewritten member — emit the block verbatim (byte-faithful).
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
            out.push(toks[i].clone());
            i += 1;
        }
        *toks = out;
        if members.is_empty() {
            return;
        }

        let mut result: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if let Tok::Word(w) = &toks[i] {
                // `instance.member` — a named-instance block.
                let qualified = toks.next_significant(i + 1).and_then(|dot| {
                    (toks[dot] == Tok::Punct('.')).then(|| toks.next_significant(dot + 1))?
                });
                if let Some(mem) = qualified {
                    if let Tok::Word(m) = &toks[mem] {
                        if let Some(found) = members.iter().find(|member| match member {
                            Member::Qualified { instance, name, .. } => instance == w && name == m,
                            Member::Bare { .. } => false,
                        }) {
                            result.push(Tok::Word(reconstruct_mat2(
                                &format!("{w}.{m}"),
                                found.columns(),
                            )));
                            i = mem + 1;
                            continue;
                        }
                    }
                }
                // A bare global — an anonymous block's member. Never rewrite an occurrence that is itself
                // a field access (`s.m`); `shadowed_bare_members` has already excluded any name a local
                // declaration could shadow.
                let is_field = toks[..i]
                    .iter()
                    .rev()
                    .find(|t| t.is_significant())
                    .is_some_and(|t| *t == Tok::Punct('.'));
                if !is_field {
                    if let Some(found) = members.iter().find(|member| match member {
                        Member::Bare { name, .. } => name == w,
                        Member::Qualified { .. } => false,
                    }) {
                        result.push(Tok::Word(reconstruct_mat2(w, found.columns())));
                        i += 1;
                        continue;
                    }
                }
            }
            result.push(toks[i].clone());
            i += 1;
        }
        *toks = result;
    }

    /// Names declared inside an ANONYMOUS std140 block that are ALSO declared somewhere else in the
    /// translation unit — a local variable or a function parameter of the same name shadows the block
    /// member, and this pass has no scope tracking to tell the two apart. Rewriting such a use would
    /// silently read the uniform where the shader meant the local, so those names are excluded from the
    /// workaround entirely and reach naga unchanged: a loud `UnsupportedMatrixTypeInStd140` beats a
    /// silently wrong value.
    ///
    /// A declaration is the name immediately preceded by a type keyword. Deliberately conservative — a
    /// false positive costs only the workaround for that one name.
    fn shadowed_bare_members(toks: &[Tok]) -> Vec<String> {
        let mut declared_in_block: Vec<String> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(b) = Std140Block::parse(toks, i) {
                    if b.instance.is_none() {
                        for seg in toks[b.lb + 1..b.rb].source().split(';') {
                            if let Some(member) = Mat2Member::parse(seg) {
                                declared_in_block.push(member.name);
                            }
                        }
                    }
                    i = b.end;
                    continue;
                }
            }
            i += 1;
        }
        if declared_in_block.is_empty() {
            return Vec::new();
        }
        // Count declarations of each candidate across the whole unit. The block's own declaration is one;
        // any second one shadows it.
        declared_in_block
            .iter()
            .filter(|name| {
                let declarations = toks
                    .iter()
                    .enumerate()
                    .filter(|(k, t)| {
                        matches!(t, Tok::Word(w) if w == *name)
                            && matches!(
                                toks[..*k].iter().rev().find(|t| t.is_significant()),
                                Some(Tok::Word(p)) if is_type_word(p)
                            )
                    })
                    .count();
                declarations > 1
            })
            .cloned()
            .collect()
    }
}

/// Whether `w` is a GLSL type keyword that would make a following identifier a DECLARATION rather than a
/// use. Deliberately broad: a false positive only costs the mat2 workaround for that name (naga then
/// reports the unsupported type), while a false negative would rewrite a shadowed local.
fn is_type_word(w: &str) -> bool {
    matches!(
        w,
        "float" | "int" | "uint" | "bool" | "double" | "void"
    ) || w.starts_with("vec")
        || w.starts_with("ivec")
        || w.starts_with("uvec")
        || w.starts_with("bvec")
        || w.starts_with("dvec")
        || w.starts_with("mat")
        || w.starts_with("dmat")
        || w.starts_with("sampler")
        || w.starts_with("texture")
        || w.starts_with("image")
}

// ---------------------------------------------------------------------------------------------------
// Comment stripping (kept local so tokenization is self-contained)
// ---------------------------------------------------------------------------------------------------
