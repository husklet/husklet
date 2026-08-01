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
                let array = member.elements.is_some();
                members.push(match instance {
                    Some(instance) => Member::Qualified {
                        instance: instance.to_string(),
                        name: member.name.clone(),
                        columns: member.columns,
                        array,
                    },
                    None => Member::Bare {
                        name: member.name.clone(),
                        columns: member.columns,
                        array,
                    },
                });
                let lead: String = seg.chars().take_while(|c| c.is_whitespace()).collect();
                // An array member flattens to one run of `elements * columns` vec4 slots.
                let slots = member.columns * member.elements.unwrap_or(1);
                format!("{lead}vec4 {}__col[{}]", member.name, slots)
            }
            _ => seg.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// A 2-row-matrix member declaration (`mat2`/`mat3x2`/`mat4x2 NAME`), scalar or ARRAY. Precision
/// qualifiers are already stripped before this pass runs.
struct Mat2Member {
    name: String,
    columns: u32,
    /// `Some(N)` for `NAME[N]`. An array is flattened into one `vec4` run of `N * columns` entries, which
    /// is byte-identical: std140 gives every matrix column its own 16-byte slot whether or not the matrix
    /// sits in an array, so element `e` column `c` lands at flat index `e * columns + c` either way.
    elements: Option<u32>,
}

impl Mat2Member {
    fn parse(seg: &str) -> Option<Self> {
        let toks = Tokens::from_source(seg);
        let sig: Vec<&Tok> = toks.iter().filter(|token| token.is_significant()).collect();
        let base = match sig.first() {
            Some(Tok::Word(w)) => w.clone(),
            _ => return None,
        };
        let matrix = MatrixShape::parse(&base)?;
        if matrix.rows != 2 {
            return None;
        }
        let name = match sig.get(1) {
            Some(Tok::Word(w)) => w.clone(),
            _ => return None,
        };
        // `NAME [ N ]` — only a literal count, since the flattened length must be computed here. An
        // unsized or expression-sized member is declined and reaches naga unchanged.
        let elements = match sig.get(2) {
            None => None,
            Some(Tok::Punct('[')) => match (sig.get(3), sig.get(4)) {
                (Some(Tok::Word(count)), Some(Tok::Punct(']'))) => Some(count.parse().ok()?),
                _ => return None,
            },
            _ => return None,
        };
        Some(Self {
            name,
            columns: matrix.columns,
            elements,
        })
    }
}

/// The reconstructed matrix rvalue for a use of a rewritten member: `matN2(P__col[0].xy, …)`, where `P`
/// is `block.member` for a named-instance block and the bare `member` for an anonymous one (whose members
/// live in global scope).
///
/// `index` is the element expression for an ARRAY member — `a[i]` reads columns at flat `i * cols + k`.
/// It is parenthesised, so an arbitrary index expression keeps its own precedence.
fn reconstruct_mat2(path: &str, cols: u32, index: Option<&str>) -> String {
    let ctor = match cols {
        3 => "mat3x2",
        4 => "mat4x2",
        _ => "mat2",
    };
    let args: Vec<String> = (0..cols)
        .map(|k| match index {
            Some(index) => format!("{path}__col[({index}) * {cols} + {k}].xy"),
            None => format!("{path}__col[{k}].xy"),
        })
        .collect();
    format!("{ctor}({})", args.join(", "))
}

/// The element subscript of a use whose name token is at `name`, and the index just past what the use
/// consumes.
///
/// A scalar member consumes only its name. An ARRAY member must be followed by `[…]`, whose inner text
/// becomes the element expression; `None` is returned when it is not, so the caller can decline rather
/// than emit a flattened read the source never asked for.
fn subscript(toks: &[Tok], name: usize, array: bool) -> Option<(Option<String>, usize)> {
    if !array {
        return Some((None, name + 1));
    }
    let lb = toks.next_significant(name + 1)?;
    if toks[lb] != Tok::Punct('[') {
        return None;
    }
    let rb = match_close(toks, lb, '[', ']');
    if rb >= toks.len() {
        return None;
    }
    Some((Some(toks[lb + 1..rb].source().trim().to_string()), rb + 1))
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
    Qualified {
        instance: String,
        name: String,
        columns: u32,
        array: bool,
    },
    Bare {
        name: String,
        columns: u32,
        array: bool,
    },
}

impl Member {
    fn columns(&self) -> u32 {
        match self {
            Self::Qualified { columns, .. } | Self::Bare { columns, .. } => *columns,
        }
    }

    /// Whether uses of this member are subscripted (`a[i]`) rather than bare.
    fn array(&self) -> bool {
        match self {
            Self::Qualified { array, .. } | Self::Bare { array, .. } => *array,
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
                            let Some((index, end)) = subscript(toks, mem, found.array()) else {
                                // An ARRAY member used without a subscript (passed whole, `.length()`):
                                // there is no flattened rvalue for that, so it is left alone and naga
                                // rejects the shader loudly rather than this pass inventing one.
                                result.push(toks[i].clone());
                                i += 1;
                                continue;
                            };
                            result.push(Tok::Word(reconstruct_mat2(
                                &format!("{w}.{m}"),
                                found.columns(),
                                index.as_deref(),
                            )));
                            i = end;
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
                        let Some((index, end)) = subscript(toks, i, found.array()) else {
                            result.push(toks[i].clone());
                            i += 1;
                            continue;
                        };
                        result.push(Tok::Word(reconstruct_mat2(
                            w,
                            found.columns(),
                            index.as_deref(),
                        )));
                        i = end;
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

// ---------------------------------------------------------------------------------------------------
// std140 arrays of scalars / 2-component vectors → arrays of 4-component vectors
// ---------------------------------------------------------------------------------------------------
//
// WGSL requires every array in the `uniform` address space to have a stride that is a multiple of 16, and
// std140 requires exactly the same thing: an array element's base alignment is rounded UP to the size of a
// `vec4`. naga's `glsl-in` does not carry that rounding into the module it builds — it types `float u[4]`
// as `array<f32, 4>` (stride 4) and `vec2 u[2]` as `array<vec2<f32>, 2>` (stride 8) — so `wgsl-out` emits
// a uniform global that wgpu's validator REFUSES:
//
//     Alignment requirements for address space Uniform are not met by [2]
//     The array stride 4 is not a multiple of the required alignment 16
//
// That is a translation defect, not a guest one: the GL driver's own std140 sizes and writes already use
// the rounded `esz + 15 & !15` element stride, so the bytes it uploads are laid out at 16 per element and
// only the declared TYPE disagrees. Declaring the member as an array of `vec4`/`ivec4`/`uvec4` and reading
// the leading component(s) back at each use describes exactly the bytes the driver already writes, so the
// rewrite is byte-faithful in both directions.
//
// Arrays of `vec3` and `vec4` (and of matrices, whose columns are `vec4`-aligned) already have a 16-byte
// stride and are left completely alone. `bool` arrays are also left alone: naga rejects a `bool` uniform
// for an unrelated reason, and rewriting one would only exchange one refusal for another.

/// The padded element type and the swizzle that recovers the original value, for an element type whose
/// std140 array stride (16) exceeds its natural WGSL stride. `None` for every element type that is already
/// 16-byte strided (`vec3`/`vec4`/matrices) or that must not be touched (`bool`, structs, samplers).
fn padded_element(base: &str) -> Option<(&'static str, &'static str)> {
    match base {
        "float" => Some(("vec4", "x")),
        "int" => Some(("ivec4", "x")),
        "uint" => Some(("uvec4", "x")),
        "vec2" => Some(("vec4", "xy")),
        "ivec2" => Some(("ivec4", "xy")),
        "uvec2" => Some(("uvec4", "xy")),
        _ => None,
    }
}

/// An array member of a std140 uniform block whose element type is narrower than the 16-byte std140 array
/// stride, and the `NAME__arr` array of 4-component vectors it is rewritten to.
struct SmallArrayMember {
    name: String,
    vector: &'static str,
    swizzle: &'static str,
}

impl SmallArrayMember {
    /// Parse `TYPE NAME[N]` out of one `;`-separated block-body segment. `None` for a non-array member, an
    /// element type that is already 16-byte strided, or any shape this pass does not model.
    fn parse(seg: &str) -> Option<Self> {
        let toks = Tokens::from_source(seg);
        let sig: Vec<&Tok> = toks.iter().filter(|token| token.is_significant()).collect();
        let base = match sig.first()? {
            Tok::Word(w) => w.as_str(),
            _ => return None,
        };
        let (vector, swizzle) = padded_element(base)?;
        let name = match sig.get(1)? {
            Tok::Word(w) => w.clone(),
            _ => return None,
        };
        if !matches!(sig.get(2)?, Tok::Punct('[')) {
            return None;
        }
        Some(Self {
            name,
            vector,
            swizzle,
        })
    }
}

impl Tokens {
    /// Rewrite every narrow-element array member of a `std140` uniform block to an equivalent array of
    /// 4-component vectors (identical std140 bytes) and swizzle the original value back at each use:
    /// `float u[4]` becomes `vec4 u__arr[4]` and `u[i]` becomes `u__arr[i].x`.
    ///
    /// A member is rewritten only when EVERY use of its name is an element subscript (`u[…]`). A use that
    /// is not — passing the whole array to a function, `u.length()` — has no swizzled equivalent this
    /// textual pass can write, so that member is left exactly as it was and wgpu refuses the module loudly
    /// (a refused shader beats a silently miscompiled one). An anonymous block's members live in global
    /// scope, so a name a local could shadow is declined on the same terms as [`Self::split_std140_mat2`].
    pub(super) fn pad_std140_arrays(&mut self) {
        let toks = &mut self.0;
        let candidates = Self::small_array_members(toks);
        if candidates.is_empty() {
            return;
        }
        let declined: Vec<String> = candidates
            .iter()
            .filter(|name| {
                Self::shadowed(toks, name) || !Self::every_use_is_subscript(toks, name)
            })
            .cloned()
            .collect();

        let mut members: Vec<SmallArrayMember> = Vec::new();
        let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(b) = Std140Block::parse(toks, i) {
                    let before = members.len();
                    let body = toks[b.lb + 1..b.rb].source();
                    let new_body = rewrite_small_arrays(&body, &declined, &mut members);
                    if members.len() == before {
                        // Nothing rewritten — emit the block verbatim (byte-faithful).
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

        // Rewrite the uses. The declarations now read `NAME__arr`, so they can no longer match `NAME`, and
        // every remaining occurrence is a subscripted read (guaranteed by `every_use_is_subscript`). The
        // subscript tokens are copied through untouched, so an arbitrary index expression survives.
        let mut result: Vec<Tok> = Vec::with_capacity(toks.len());
        let mut i = 0;
        while i < toks.len() {
            if let Tok::Word(w) = &toks[i] {
                if let Some(member) = members.iter().find(|member| &member.name == w) {
                    if let Some(lb) = toks.next_significant(i + 1) {
                        if toks[lb] == Tok::Punct('[') {
                            let rb = match_close(toks, lb, '[', ']');
                            if rb < toks.len() {
                                result.push(Tok::Word(format!("{w}__arr")));
                                result.extend(toks[i + 1..=rb].iter().cloned());
                                result.push(Tok::Punct('.'));
                                result.push(Tok::Word(member.swizzle.to_string()));
                                i = rb + 1;
                                continue;
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

    /// Every narrow-element array member declared in any `std140` uniform block in the unit.
    fn small_array_members(toks: &[Tok]) -> Vec<String> {
        let mut names = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], Tok::Word(w) if w == "layout") {
                if let Some(b) = Std140Block::parse(toks, i) {
                    for seg in toks[b.lb + 1..b.rb].source().split(';') {
                        if let Some(member) = SmallArrayMember::parse(seg) {
                            names.push(member.name);
                        }
                    }
                    i = b.end;
                    continue;
                }
            }
            i += 1;
        }
        names
    }

    /// Whether `name` is declared more than once in the unit — a local or parameter shadowing the block
    /// member this pass has no scope tracking to distinguish. See [`Self::shadowed_bare_members`].
    fn shadowed(toks: &[Tok], name: &str) -> bool {
        toks.iter()
            .enumerate()
            .filter(|(k, t)| {
                matches!(t, Tok::Word(w) if w == name)
                    && matches!(
                        toks[..*k].iter().rev().find(|t| t.is_significant()),
                        Some(Tok::Word(p)) if is_type_word(p)
                    )
            })
            .count()
            > 1
    }

    /// Whether every non-declaration occurrence of `name` is immediately followed by `[` — the only use
    /// form the swizzle rewrite can express.
    fn every_use_is_subscript(toks: &[Tok], name: &str) -> bool {
        (0..toks.len())
            .filter(|k| matches!(&toks[*k], Tok::Word(w) if w == name))
            .filter(|k| {
                !matches!(
                    toks[..*k].iter().rev().find(|t| t.is_significant()),
                    Some(Tok::Word(p)) if is_type_word(p)
                )
            })
            .all(|k| {
                toks.next_significant(k + 1)
                    .is_some_and(|next| toks[next] == Tok::Punct('['))
            })
    }
}

/// Rewrite the body text of a std140 uniform block: each narrow-element array member `TYPE NAME[N]`
/// becomes `VEC4TYPE NAME__arr[N]`, recording it in `members`. Every other member is kept verbatim, and a
/// `declined` name is left exactly as it was.
fn rewrite_small_arrays(
    body: &str,
    declined: &[String],
    members: &mut Vec<SmallArrayMember>,
) -> String {
    body.split(';')
        .map(|seg| match SmallArrayMember::parse(seg) {
            Some(member) if !declined.contains(&member.name) => {
                let lead: String = seg.chars().take_while(|c| c.is_whitespace()).collect();
                let bracket = match seg.find('[') {
                    Some(bracket) => bracket,
                    None => return seg.to_string(),
                };
                let text = format!(
                    "{lead}{} {}__arr{}",
                    member.vector,
                    member.name,
                    &seg[bracket..]
                );
                members.push(member);
                text
            }
            _ => seg.to_string(),
        })
        .collect::<Vec<_>>()
        .join(";")
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
