use super::*;

/// Slice out `(param_list, body)` for `.entry <entry>` (or `.visible .entry <entry>`).
impl Ptx {
    pub(super) fn extract_entry(source: &str, entry: &str) -> Result<(String, String)> {
        let bytes = source;
        let mut search = 0usize;
        loop {
            let idx = bytes[search..]
                .find(".entry")
                .ok_or_else(|| Self::error(format!("entry `{entry}` not found")))?
                + search;
            let after = &bytes[idx + ".entry".len()..];
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if name == entry {
                let rest = &bytes[idx..];
                let lp = rest.find('(');
                let lb = rest
                    .find('{')
                    .ok_or_else(|| Self::error(format!("entry `{entry}` has no body")))?;
                // a param list exists only if '(' precedes '{'
                let params_src = if let Some(lp) = lp {
                    if lp < lb {
                        let rp = rest[lp..]
                            .find(')')
                            .ok_or_else(|| Self::error("unterminated param list"))?
                            + lp;
                        rest[lp + 1..rp].to_string()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                let body = Self::matched_braces(&rest[lb..])?;
                return Ok((params_src, body));
            }
            search = idx + ".entry".len();
        }
    }

    /// Return the substring inside the first `{...}` (brace-matched), excluding the outer braces.
    pub(super) fn matched_braces(s: &str) -> Result<String> {
        let mut depth = 0i32;
        let mut start = None;
        for (i, c) in s.char_indices() {
            match c {
                '{' => {
                    if depth == 0 {
                        start = Some(i + 1);
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let st = start.unwrap();
                        return Ok(s[st..i].to_string());
                    }
                }
                _ => {}
            }
        }
        Err(Self::error("unterminated kernel body"))
    }

    /// Parse `.param .TYPE name` entries (comma-separated) into `(name, width)`.
    pub(super) fn parse_params(src: &str) -> Result<Vec<(String, u32)>> {
        let mut out = Vec::new();
        for raw in src.split(',') {
            let s = raw.trim();
            if s.is_empty() {
                continue;
            }
            let toks: Vec<&str> = s.split_whitespace().collect();
            if toks.is_empty() || toks[0] != ".param" {
                return Err(Self::error(format!("bad .param decl: `{s}`")));
            }
            let ty = toks
                .iter()
                .skip(1)
                .find(|t| t.starts_with('.'))
                .ok_or_else(|| Self::error(format!("param missing type: `{s}`")))?;
            let width = Self::type_width(ty)?;
            let name_tok = *toks.last().unwrap();
            if name_tok.starts_with('.') {
                return Err(Self::error(format!("param missing name: `{s}`")));
            }
            let name: String = name_tok
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if name.is_empty() {
                return Err(Self::error(format!("param missing name: `{s}`")));
            }
            if name_tok.contains('[') {
                return Err(Self::error(format!(
                    "array/struct param unsupported: `{s}`"
                )));
            }
            out.push((name, width));
        }
        Ok(out)
    }

    pub(super) fn type_width(ty: &str) -> Result<u32> {
        Ok(match ty {
            ".u8" | ".s8" | ".b8" => 1,
            ".u16" | ".s16" | ".b16" | ".f16" => 2,
            ".u32" | ".s32" | ".b32" | ".f32" => 4,
            ".u64" | ".s64" | ".b64" | ".f64" => 8,
            other => return Err(Self::error(format!("unsupported param type `{other}`"))),
        })
    }
}
