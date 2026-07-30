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

    /// Lower the GLSL ES texture builtins onto their desktop overloads. The explicit-LOD ES forms
    /// (`texture2DLod`/`textureCubeLod`, GLSL ES 1.00 §8.7 vertex-shader lookups) map to `textureLod` and must
    /// be rewritten BEFORE the implicit forms, whose names are their prefixes.
    pub(super) fn lower_texture_builtins(&mut self) {
        for (from, to) in [
            ("texture2DProjLod(", "textureProjLod("),
            ("texture2DLod(", "textureLod("),
            ("textureCubeLod(", "textureLod("),
            ("texture2DProj(", "textureProj("),
            ("texture2D(", "texture("),
            ("textureCube(", "texture("),
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
