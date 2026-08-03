//! GLSL ES → desktop GLSL source normalisation: the two rewrites that are about the ES DIALECT rather than
//! about the interface — precision qualifiers and the texture builtins.

use super::*;

/// Remove ES precision qualifiers from a shader body — invalid as qualifiers in desktop core GLSL.
pub(super) struct NormalizedSource<'a> {
    text: &'a mut String,
}

impl<'a> NormalizedSource<'a> {
    pub(super) fn new(text: &'a mut String) -> Self {
        Self { text }
    }

    /// ES precision qualifiers are REMOVED rather than carried: naga's desktop `glsl-in` does not accept them
    /// as qualifiers, and the host executes every stage on Metal through wgpu, which has no relaxed-precision
    /// numeric type — `mediump`/`lowp` would widen to the `highp` behaviour anyway (GLSL ES 1.00 §4.5.2 permits
    /// any precision at least as high as requested). Macros are expanded first, so a `#define`d qualifier is a
    /// real `highp`/`mediump` token by the time this runs.
    pub(super) fn strip_precision(&mut self) {
        wreplace(self.text, "lowp", "");
        wreplace(self.text, "mediump", "");
        wreplace(self.text, "highp", "");
    }

    /// Remove ES `const` from function input parameters before desktop naga sees them.
    ///
    /// GLSL ES 1.00 permits `const in TYPE x` and `const TYPE x` parameter declarations.
    /// Naga 24 models `const` and `in` as competing storage qualifiers and also parses a
    /// bare `const` parameter as a global-style constant requiring an initializer. The
    /// guest compiler has already enforced the ES parameter grammar and const assignment
    /// rules. Dropping `const` here preserves the call ABI and value semantics: an input
    /// parameter remains a private copy, while `out` and `inout` are left untouched.
    pub(super) fn lower_const_parameters(&mut self) {
        let source = self.text.clone();
        let bytes = source.as_bytes();
        let Some(body) = bytes.iter().position(|byte| *byte == b'{') else {
            return;
        };
        let mut depth = 0usize;
        let mut open = None;
        for (at, byte) in bytes[..body].iter().enumerate() {
            match byte {
                b'(' => {
                    if depth == 0 {
                        open = Some(at);
                    }
                    depth += 1;
                }
                b')' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let Some(open) = open else {
            return;
        };
        let mut depth = 1usize;
        let mut close = None;
        for (relative, byte) in bytes[open + 1..body].iter().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + 1 + relative);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            return;
        };
        let mut edits = Vec::new();
        let mut at = open + 1;
        while at < close {
            let Some(relative) = source[at..close].find("const") else {
                break;
            };
            let start = at + relative;
            let end = start + "const".len();
            let before = start
                .checked_sub(1)
                .map(|index| bytes[index])
                .unwrap_or(b' ');
            let after = bytes.get(end).copied().unwrap_or(b' ');
            if !Tokens::is_word(before) && !Tokens::is_word(after) {
                edits.push((start, end));
            }
            at = end;
        }
        for (start, end) in edits.into_iter().rev() {
            self.text.replace_range(start..end, "");
        }
    }

    /// GLES2 exposes a single color attachment, but permits it to be addressed through
    /// `gl_FragData[index]`. Every defined index therefore resolves to attachment zero. Desktop GLSL and
    /// WGSL have no dynamically indexed fragment-output array, so redirect the whole indexed lvalue to the
    /// synthesized location-zero output. An out-of-range runtime index is undefined by GLES and needs no
    /// distinct host representation.
    pub(super) fn lower_single_output_frag_data(&mut self, output: &str) {
        let source = self.text.clone();
        let bytes = source.as_bytes();
        let name = b"gl_FragData";
        let mut edits = Vec::new();
        let mut cursor = 0usize;
        while let Some(relative) = source[cursor..].find("gl_FragData") {
            let start = cursor + relative;
            cursor = start + name.len();
            let before = start.checked_sub(1).map(|at| bytes[at]).unwrap_or(b' ');
            let after = bytes.get(cursor).copied().unwrap_or(b' ');
            if Tokens::is_word(before) || Tokens::is_word(after) {
                continue;
            }
            let mut open = cursor;
            while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                open += 1;
            }
            if bytes.get(open) != Some(&b'[') {
                continue;
            }
            let mut depth = 1usize;
            let mut close = open + 1;
            while close < bytes.len() && depth != 0 {
                match bytes[close] {
                    b'[' => depth += 1,
                    b']' => depth -= 1,
                    _ => {}
                }
                close += 1;
            }
            if depth == 0 {
                edits.push((start, close));
                cursor = close;
            }
        }
        for (start, end) in edits.into_iter().rev() {
            self.text.replace_range(start..end, output);
        }
    }

    /// Lower the GLSL ES texture builtins onto their desktop overloads. The explicit-LOD ES forms
    /// (`texture2DLod`/`textureCubeLod`, GLSL ES 1.00 §8.7 vertex-shader lookups) map to `textureLod` and must
    /// be rewritten BEFORE the implicit forms, whose names are their prefixes.
    pub(super) fn lower_texture_builtins(&mut self) {
        for (from, to) in [
            ("texture2DProjLod(", "textureProjLod("),
            ("texture2DLod(", "textureLod("),
            ("textureCubeLod(", "textureLod("),
            ("texture3DLod(", "textureLod("),
            ("texture2DProj(", "textureProj("),
            ("texture2D(", "texture("),
            ("textureCube(", "texture("),
            ("texture3D(", "texture("),
        ] {
            sreplace(self.text, from, to);
        }
    }

    /// GLSL ES 1.00 §8.7: a texture lookup in a VERTEX shader has no implicit derivatives — the ES builtin
    /// samples at LOD 0. naga's validator rejects the derivative-taking `texture()` inside a vertex entry
    /// point (`ForbiddenStageOperations`), so every two-argument lookup in the vertex stage becomes
    /// `textureLod(sampler, coord, 0.0)`. A lookup that already passes an explicit LOD is left alone.
    pub(super) fn pin_vertex_lod(&mut self) {
        let source = self.text.clone();
        let bytes = source.as_bytes();
        let mut edits = Vec::new();
        let mut at = 0usize;
        while let Some(relative) = source[at..].find("texture(") {
            let call = at + relative;
            at = call + "texture(".len();
            if call > 0 && Tokens::is_word(bytes[call - 1]) {
                continue;
            }
            let mut cursor = at;
            let mut depth = 0usize;
            let mut commas = 0usize;
            let close = loop {
                match bytes.get(cursor) {
                    None => break None,
                    Some(b'(' | b'[') => depth += 1,
                    Some(b')') if depth == 0 => break Some(cursor),
                    Some(b')' | b']') => depth = depth.saturating_sub(1),
                    Some(b',') if depth == 0 => commas += 1,
                    Some(_) => {}
                }
                cursor += 1;
            };
            let Some(close) = close else { continue };
            if commas == 1 {
                edits.push((call, close));
            }
            at = close + 1;
        }
        for (call, close) in edits.into_iter().rev() {
            self.text.insert_str(close, ", 0.0");
            self.text
                .replace_range(call..call + "texture".len(), "textureLod");
        }
    }
}
