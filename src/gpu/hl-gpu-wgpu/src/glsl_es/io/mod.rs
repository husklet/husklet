use super::*;

/// A split aggregate interface member: `matCxR` columns or a `vec[N]` array, per-slot vector type, and the
/// spelling of a private global of the whole aggregate type.
enum AggTy {
    /// `matCxR` → `cols` columns, each a `col_ty` (`vec{rows}`); global declared with the matrix token.
    Matrix {
        tok: String,
        cols: u32,
        col_ty: String,
    },
    /// `elem[count]` array; global declared as `elem name[count]`.
    Array { elem: String, count: u32 },
}

impl AggTy {
    /// (slot count, per-slot vector type, private-global declaration for `name`).
    fn parts(&self, name: &str) -> (u32, String, String) {
        match self {
            AggTy::Matrix { tok, cols, col_ty } => {
                (*cols, col_ty.clone(), format!("{tok} {name};"))
            }
            AggTy::Array { elem, count } => {
                (*count, elem.clone(), format!("{elem} {name}[{count}];"))
            }
        }
    }
}

/// A parsed `IN(_loc) TYPE name;` / `PASS(_loc) …` / `PASS_FLAT(_loc) …` interface declaration.
struct IoDecl {
    macro_name: String, // IN | PASS | PASS_FLAT
    loc: String,        // the `_loc` argument text (numeric for a real split)
    agg: Option<AggTy>, // Some(_) only for a matrix/array member; None leaves the decl untouched
    name: String,
    end: usize, // token index just past the terminating `;`
}

pub(super) struct MatrixShape {
    pub(super) columns: u32,
    pub(super) rows: u32,
}

impl MatrixShape {
    /// Parse `matCxR` / `matN`; reject non-matrix words (`material`, `mat`, `vec4`, …).
    pub(super) fn parse(token: &str) -> Option<Self> {
        let rest = token.strip_prefix("mat")?;
        let mut dimensions = rest.split('x');
        let columns: u32 = dimensions.next()?.parse().ok()?;
        let rows = match dimensions.next() {
            Some(rows) => rows.parse().ok()?,
            None => columns,
        };
        if dimensions.next().is_some() {
            return None;
        }
        Some(Self { columns, rows })
    }
}

/// Collect object-like `#define NAME BODY` type aliases (e.g. GskGpu's `#define RoundedRect vec4[3]`) so an
/// aggregate interface member declared through the alias is recognized. Only the alias *body text* is kept.
struct TypeAliases(std::collections::HashMap<String, String>);

impl TypeAliases {
    fn from_tokens(tokens: &[Tok]) -> Self {
        let mut aliases = std::collections::HashMap::new();
        for token in tokens {
            if let Tok::Pp(preprocessor) = token {
                if let Some(rest) = preprocessor
                    .trim_start()
                    .strip_prefix('#')
                    .map(str::trim_start)
                {
                    if let Some(rest) = rest.strip_prefix("define") {
                        if rest.starts_with(char::is_whitespace) {
                            let rest = rest.trim_start();
                            let end = rest.find(|character: char| {
                                !(character == '_' || character.is_ascii_alphanumeric())
                            });
                            if let Some(end) = end {
                                let (name, body) = rest.split_at(end);
                                if body.starts_with(char::is_whitespace) && !name.is_empty() {
                                    aliases.insert(name.to_owned(), body.trim().to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
        Self(aliases)
    }

    fn classify(&self, tail: &str) -> Option<(Option<AggTy>, String)> {
        let expanded = {
            let first_end = tail
                .find(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
                .unwrap_or(tail.len());
            let (first, rest) = tail.split_at(first_end);
            match self.0.get(first) {
                Some(body) => format!("{body}{rest}"),
                None => tail.to_owned(),
            }
        };
        let tokens = Tokens::from_source(&expanded);
        let significant: Vec<&Tok> = tokens
            .iter()
            .filter(|token| token.is_significant())
            .collect();
        let base = match significant.first() {
            Some(Tok::Word(word)) => word.clone(),
            _ => return None,
        };
        let name = significant.iter().skip(1).find_map(|token| match token {
            Tok::Word(word) if word.parse::<u32>().is_err() => Some(word.clone()),
            _ => None,
        })?;
        let count = significant.iter().enumerate().find_map(|(index, token)| {
            matches!(token, Tok::Punct('['))
                .then(|| significant.get(index + 1))
                .flatten()
                .and_then(|token| match token {
                    Tok::Word(number) => number.parse().ok(),
                    _ => None,
                })
        });
        let aggregate = if let Some(matrix) = MatrixShape::parse(&base) {
            Some(AggTy::Matrix {
                tok: base.clone(),
                cols: matrix.columns,
                col_ty: format!("vec{}", matrix.rows),
            })
        } else {
            count.map(|count| AggTy::Array { elem: base, count })
        };
        Some((aggregate, name))
    }
}

/// Parse an `IN/PASS/PASS_FLAT (_loc) TYPE name [array];` declaration whose macro word is at `i`, resolving
/// a type alias for the aggregate classification. `None` if the shape does not match (left untouched).
fn parse_io_decl(toks: &[Tok], i: usize, aliases: &TypeAliases) -> Option<IoDecl> {
    let macro_name = match &toks[i] {
        Tok::Word(w) => w.clone(),
        _ => return None,
    };
    let lp = toks.next_significant(i + 1)?;
    if toks[lp] != Tok::Punct('(') {
        return None;
    }
    let rp = match_close(toks, lp, '(', ')');
    if rp >= toks.len() {
        return None;
    }
    let loc = toks[lp + 1..rp].source().trim().to_string();
    // Everything from after `)` to the terminating `;` is `TYPE name [array]`.
    let mut e = rp + 1;
    while e < toks.len() && toks[e] != Tok::Punct(';') {
        e += 1;
    }
    if e >= toks.len() {
        return None;
    }
    let tail = toks[rp + 1..e].source();
    let (agg, name) = aliases.classify(tail.trim())?;
    Some(IoDecl {
        macro_name,
        loc,
        agg,
        name,
        end: e + 1,
    })
}

/// Classify a `TYPE name [array]` interface tail (after alias expansion) into `(aggregate?, name)`. An
/// aggregate is a `matCxR` (→ per-column vectors) or an array (→ per-element slots); a scalar/vector member
/// returns `(None, name)` and is left untouched. Shared by the GskGpu macro path ([`parse_io_decl`]) and the
/// raw `layout(location=N) in/out …` ANGLE path ([`parse_raw_io_decl`]).
/// Parse a RAW ANGLE-style interface declaration `layout(location = N[, …]) [flat] (in|out) TYPE name
/// [array];` whose `layout` word is at `i`. This is the non-macro sibling of [`parse_io_decl`]: ANGLE
/// emits explicit `layout(location=N) in mat4 aModel;` / `out vec4 v[3];` (a matrix attribute or an array
/// varying) that naga rejects as a single located slot (`NotIOShareableType`), where GskGpu hid the same
/// shapes behind `IN`/`PASS` macros. Returns `None` for a scalar/vector member, a `uniform`/sampler block
/// (no `in`/`out`), a missing `location`, or a fragment `out` (a color target, never split). The direction
/// is encoded as the SAME synthetic macro name `build_io_split` already interprets, so the split/bridge
/// logic is shared verbatim.
fn parse_raw_io_decl(
    toks: &[Tok],
    i: usize,
    is_vertex: bool,
    aliases: &TypeAliases,
) -> Option<IoDecl> {
    let lp = toks.next_significant(i + 1)?;
    if toks[lp] != Tok::Punct('(') {
        return None;
    }
    let rp = match_close(toks, lp, '(', ')');
    if rp >= toks.len() {
        return None;
    }
    let loc = LayoutQualifier::parse(&toks[lp + 1..rp].source()).location()?;
    // After `)`: an optional `flat` interpolation qualifier, then the `in`/`out` storage qualifier.
    let mut j = toks.next_significant(rp + 1)?;
    let flat = matches!(&toks[j], Tok::Word(w) if w == "flat");
    if flat {
        j = toks.next_significant(j + 1)?;
    }
    let kw = match &toks[j] {
        Tok::Word(w) if w == "in" || w == "out" => w.clone(),
        _ => return None, // `uniform`, `buffer`, a type — not an interface in/out decl
    };
    // A fragment `out` is a color attachment (vec4), never an aggregate we split.
    if !is_vertex && kw == "out" {
        return None;
    }
    let mut e = j + 1;
    while e < toks.len() && toks[e] != Tok::Punct(';') {
        e += 1;
    }
    if e >= toks.len() {
        return None;
    }
    let tail = toks[j + 1..e].source();
    let (agg, name) = aliases.classify(tail.trim())?;
    // Map direction to the synthetic macro name `build_io_split` understands: a vertex input is an
    // attribute (`IN`); every other direction is a varying (`PASS`/`PASS_FLAT`), whose in/out sense
    // `build_io_split` derives from the stage.
    let macro_name = if is_vertex && kw == "in" {
        "IN"
    } else if flat {
        "PASS_FLAT"
    } else {
        "PASS"
    };
    Some(IoDecl {
        macro_name: macro_name.to_string(),
        loc,
        agg,
        name,
        end: e + 1,
    })
}

/// Rewrite dual-source-blend fragment outputs (`layout(location = L, index = X) out vec4 name;`) into a form
/// naga's `glsl-in` can parse. naga rejects the `index=` layout qualifier outright ("Unexpected qualifier")
/// and always emits `second_blend_source: false`, yet its IR and `wgsl-out` DO model dual-source blending
/// (`@second_blend_source`). So: strip the `index=` qualifier from every such `layout(...)`, and rename each
/// `index >= 1` output (declaration AND uses) with [`BLEND_SRC1_SUFFIX`]. Both sources then carry the same
/// `location = L`; the module post-pass [`crate::wgsl::fix_dual_source_blend`] flips `second_blend_source` on
/// the suffixed fragment-output member before validation, so the two same-location outputs are the legal
/// dual-source pair rather than a binding collision.
struct LayoutQualifier {
    location: Option<String>,
    index: Option<u32>,
}

impl LayoutQualifier {
    fn parse(source: &str) -> Self {
        let tokens = Tokens::from_source(source);
        let mut qualifier = Self {
            location: None,
            index: None,
        };
        for (position, token) in tokens.iter().enumerate() {
            let Tok::Word(name) = token else {
                continue;
            };
            if name != "location" && name != "index" {
                continue;
            }
            let Some(equal) = tokens.next_significant(position + 1) else {
                continue;
            };
            if tokens[equal] != Tok::Punct('=') {
                continue;
            }
            let Some(value) = tokens.next_significant(equal + 1) else {
                continue;
            };
            let Tok::Word(value) = &tokens[value] else {
                continue;
            };
            if name == "location" && value.parse::<i64>().is_ok() {
                qualifier.location = Some(value.clone());
            } else if name == "index" {
                qualifier.index = value.parse().ok();
            }
        }
        qualifier
    }

    fn location(&self) -> Option<String> {
        self.location.clone()
    }

    fn dual_source(&self) -> Option<(String, u32)> {
        self.index.map(|index| {
            (
                self.location.clone().unwrap_or_else(|| "0".to_owned()),
                index,
            )
        })
    }
}

pub(super) mod rewrite;
