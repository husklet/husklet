use super::*;

impl Ptx {
    pub(super) fn parse_body(
        body: &str,
        params: &[(String, u32)],
        interner: &mut Interner,
    ) -> Result<RawFn> {
        let flat = Ptx::strip_comments(body).replace(['\n', '\r'], " ");
        let raw_stmts: Vec<&str> = flat.split(';').collect();

        let mut labels: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut stmts: Vec<String> = Vec::new();
        let mut shared_syms: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        let mut shared_cursor: u32 = 0;
        for stmt in raw_stmts {
            let mut s = stmt.trim().to_string();
            if s.is_empty() {
                continue;
            }
            // peel any number of leading `label:` tokens.
            loop {
                let mut it = s.splitn(2, char::is_whitespace);
                let first = it.next().unwrap_or("");
                if let Some(name) = first.strip_suffix(':') {
                    labels.insert(name.to_string(), stmts.len() as u32);
                    s = it.next().unwrap_or("").trim().to_string();
                    if s.is_empty() {
                        break;
                    }
                } else {
                    break;
                }
            }
            if s.is_empty() {
                continue;
            }
            // A `.shared` state-space variable declaration — either bare (`.shared …`) or, as nvcc actually
            // emits it, behind linkage qualifiers (`.extern .shared …`, `.visible .shared …`). It is a
            // directive line (starts with `.`) with a standalone `.shared` token; an instruction that touches
            // shared memory (`ld.shared.f32`, `st.shared.b32`) carries `.shared` inside its opcode token, never
            // as a bare token, so it is not misrouted here. Routing the qualified form matters: `parse_shared_decl`
            // is what rejects the dynamic (`extern`, unsized `name[]`) form — skipping it would silently model a
            // dynamic-shared kernel as having zero shared memory (wrong results / a deferred OOB) instead of an
            // honest `GpuError::Kernel`.
            if s.starts_with('.') {
                if s.split_whitespace().any(|t| t == ".shared") {
                    Self::parse_shared_decl(&s, &mut shared_syms, &mut shared_cursor)?;
                }
                continue;
            }
            stmts.push(s);
        }
        let shared_bytes = Ptx::align(shared_cursor, 4);

        let param_idx: std::collections::HashMap<&str, u16> = params
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.as_str(), i as u16))
            .collect();

        let mut sregs = SRegAlloc::default();
        let mut insts = Vec::with_capacity(stmts.len());
        let mut ld_param = Vec::new();
        for s in &stmts {
            let inst =
                Self::parse_inst(s, &param_idx, &labels, &shared_syms, interner, &mut sregs)?;
            if let Inst::LdParam { d, param } = inst {
                ld_param.push((d, param));
            }
            insts.push(inst);
        }

        if interner.is_overflowed() {
            return Err(Ptx::error(
                "kernel names more than 65535 registers — outside the modeled register file",
            ));
        }

        // Materialize operand-position special registers: emit their `MovSReg` prelude at the top of the
        // stream, then shift every branch target past it. Branch targets are instruction indices (== statement
        // ordinals here), so they move by the prelude length; `ld_param` seeds are register indices, unaffected.
        let prelude = sregs.prelude();
        if !prelude.is_empty() {
            let n = prelude.len() as u32;
            for inst in &mut insts {
                if let Inst::Bra { target, .. } = inst {
                    *target += n;
                }
            }
            let mut all = prelude;
            all.extend(insts);
            insts = all;
        }

        Ok(RawFn {
            insts,
            reg_count: interner.count(),
            ld_param,
            shared_bytes,
        })
    }

    /// Parse a `.shared` declaration, reserving space + recording the symbol's base byte offset. Dynamic
    /// `extern` shared (`name[]`, sized at launch) is rejected — out of the statically-sized subset.
    fn parse_shared_decl(
        s: &str,
        syms: &mut std::collections::HashMap<String, u32>,
        cursor: &mut u32,
    ) -> Result<()> {
        let toks: Vec<&str> = s.split_whitespace().collect();
        let ty = toks
            .iter()
            .skip(1)
            .find(|t| t.starts_with('.') && *t != &".align" && *t != &".shared")
            .copied();
        let name_tok = *toks
            .last()
            .ok_or_else(|| Ptx::error(format!("bad .shared decl: `{s}`")))?;
        let elem = ty.map(Ptx::type_width).transpose()?.unwrap_or(1);
        let (name, count) = if let Some(lb) = name_tok.find('[') {
            let name = &name_tok[..lb];
            let inner = name_tok[lb + 1..].trim_end_matches([']', ';']).trim();
            if inner.is_empty() {
                return Err(Ptx::error(format!(
                    "dynamic (extern) .shared unsupported: `{s}`"
                )));
            }
            let count: u32 = Ptx::parse_imm_i(inner)
                .map_err(|_| Ptx::error(format!("bad .shared array size: `{s}`")))?
                as u32;
            (name, count)
        } else {
            (name_tok.trim_end_matches(';'), 1u32)
        };
        let name: String = name
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if name.is_empty() {
            return Err(Ptx::error(format!(".shared decl missing name: `{s}`")));
        }
        *cursor = Ptx::align(*cursor, elem);
        syms.insert(name, *cursor);
        *cursor += elem.saturating_mul(count.max(1));
        Ok(())
    }
}
