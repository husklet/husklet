use super::*;

pub(super) struct Tokens<'a>(pub(super) &'a str);

impl<'a> Tokens<'a> {
    pub(super) fn first_word(&self) -> &'a str {
        let u = self.0;
        u.split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
    }

    /// Whether a texture builtin's sampler expression names this exact reflected sampler.
    ///
    /// Translated GLES uses `sampler2D(name_hltex, name_hlsmp)` while verbatim desktop GLSL may retain the
    /// direct name. Match those exact forms; substring matching confuses overlapping names such as `image`
    /// and `images`, applying one texture object's orientation to another.
    pub(super) fn names_sampler(&self, name: &str) -> bool {
        let expression = self.0.trim();
        if expression == name {
            return true;
        }
        let binding = if let Some(open) = name.rfind('[') {
            name.get(open + 1..name.len().saturating_sub(1))
                .and_then(|element| {
                    let base = &name[..open];
                    (!base.is_empty()
                        && !element.is_empty()
                        && element.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| format!("{base}_{element}"))
                })
                .unwrap_or_else(|| name.to_string())
        } else {
            name.to_string()
        };
        let Some(open) = expression.find('(') else {
            return expression == binding;
        };
        let constructor = expression[..open].trim();
        if !constructor.starts_with("sampler") || !expression.ends_with(')') {
            return false;
        }
        let arguments = &expression[open + 1..expression.len() - 1];
        let mut depth = 0usize;
        let comma = arguments.char_indices().find_map(|(at, ch)| match ch {
            '(' | '[' | '{' => {
                depth += 1;
                None
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                None
            }
            ',' if depth == 0 => Some(at),
            _ => None,
        });
        let Some(comma) = comma else {
            return false;
        };
        arguments[..comma].trim() == format!("{binding}_hltex")
            && arguments[comma + 1..].trim() == format!("{binding}_hlsmp")
    }
}

/// A leading qualifier/keyword whose top-level unit is an INTERFACE declaration the translator REGENERATES
/// (attributes/varyings/uniforms) or a `precision`/`const` statement handled elsewhere — so it is NOT carried
/// verbatim. Everything else at top level (a `struct`, a helper function, a plain global) IS carried.
/// Partition a stage's carried-through globals into `(struct_defs, functions_and_globals)`, in source order.
/// The interface declarations the translator regenerates (attribute/varying/uniform/const/precision) are
/// dropped here; struct definitions are separated so they can be emitted BEFORE the uniform block and helper
/// functions that reference them. `main` is excluded (its body is emitted by [`main_body`]). This is the fix
/// for ANGLE/real-world shaders that declare helper functions, `struct`s, array-of-struct locals, etc. — the
/// old path kept only `main`, so any such shader failed to compile with "unknown function/type".
impl Source<'_> {
    pub(super) fn partition_globals(self) -> (Vec<String>, Vec<String>) {
        let stripped = self.without_preprocessor();
        let mut structs = Vec::new();
        let mut funcs = Vec::new();
        for u in Source::new(&stripped).top_level_units() {
            let fw = Tokens(&u).first_word();
            if TypeToken(fw).is_regenerated_qualifier() {
                continue;
            }
            if fw == "struct" {
                structs.push(u);
                continue;
            }
            // A function definition/prototype whose name is `main` is emitted by `main_body`, not carried.
            if let Some(paren) = u.find('(') {
                let before_brace = u.find('{').is_none_or(|bp| paren < bp);
                if before_brace {
                    let name = u[..paren].trim_end();
                    let name = name
                        .rsplit(|c: char| c.is_whitespace() || c == '*')
                        .next()
                        .unwrap_or("");
                    if name == "main" {
                        continue;
                    }
                }
            }
            funcs.push(u);
        }
        (structs, funcs)
    }
}

/// Whether a varying/interface type is an integer/bool aggregate that desktop GLSL (and naga) REQUIRE to
/// carry the `flat` interpolation qualifier — an `int`/`uint`/`bool` (or `ivecN`/`uvecN`/`bvecN`) varying
/// cannot be smoothly interpolated. GLSL-ES declares these `flat`, but the reflection [`collect`] drops the
/// qualifier, so the regenerated declaration must re-add it or the stage fails to compile.
/// Collect uniforms from vs+fs (dedup by name), split into DATA uniforms and SAMPLER uniforms
/// (gl_shim.c `collect_uniforms`).
pub(super) struct Declarations<'a> {
    vertex: &'a str,
    fragment: &'a str,
}

impl<'a> Declarations<'a> {
    pub(super) fn from_stages(vertex: &'a str, fragment: &'a str) -> Self {
        Self { vertex, fragment }
    }

    pub(super) fn uniforms(self) -> (Vec<Decl>, Vec<Decl>) {
        let mut all: Vec<Decl> = Vec::new();
        for d in Tokens(self.vertex)
            .collect("uniform")
            .into_iter()
            .chain(Tokens(self.fragment).collect("uniform"))
        {
            // GskGpu emits an external-image declaration and a sampler2D fallback with the same name in
            // opposite preprocessor branches. External images are not part of this GL model yet; the host
            // preprocessor selects the ordinary sampler branch. Do not let the inactive declaration shadow
            // that live fallback during source-level reflection.
            if d.ty == "samplerExternalOES" {
                continue;
            }
            if !all.iter().any(|x| x.name == d.name) {
                all.push(d);
            }
        }
        let (mut data, mut samps) = (Vec::new(), Vec::new());
        for d in all {
            if d.is_sampler() {
                samps.push(d);
            } else {
                data.push(d);
            }
        }
        (data, samps)
    }
}

pub(super) fn append_decls_unique(dst: &mut Vec<Decl>, src: Vec<Decl>, max: usize) {
    for d in src {
        if dst.len() >= max {
            break;
        }
        if !dst.iter().any(|x| x.name == d.name) {
            dst.push(d);
        }
    }
}

/// Vertex attributes: `attribute` decls + `in` decls (unique by name) — gl_shim.c
/// `collect_vertex_attrs`.
/// MSL struct member (size, align) for a GLSL uniform type (gl_shim.c `msl_type_layout`).
impl TypeToken<'_> {
    /// `(std140 size, alignment, scalar components)`.
    pub(super) fn layout(&self) -> Option<(usize, usize, usize)> {
        Some(match self.0 {
            "float" | "int" | "uint" | "bool" => (4, 4, 1),
            "vec2" | "ivec2" | "uvec2" | "bvec2" => (8, 8, 2),
            // Keep vec3's occupied span at 16 bytes. This is conservative across std140 implementations and
            // prevents a following member from aliasing the fourth lane.
            "vec3" | "ivec3" | "uvec3" | "bvec3" => (16, 16, 3),
            "vec4" | "ivec4" | "uvec4" | "bvec4" => (16, 16, 4),
            // std140 matrices are arrays of column vectors; every column has a 16-byte stride.
            "mat2" | "mat2x2" => (32, 16, 4),
            "mat3" | "mat3x3" => (48, 16, 9),
            "mat4" | "mat4x4" => (64, 16, 16),
            "mat2x3" => (32, 16, 6),
            "mat2x4" => (32, 16, 8),
            "mat3x2" => (48, 16, 6),
            "mat3x4" => (48, 16, 12),
            "mat4x2" => (64, 16, 8),
            "mat4x3" => (64, 16, 12),
            _ => return None,
        })
    }

    /// The std140 distance in bytes between two columns of this type, or `0` when it is not a matrix.
    ///
    /// This is what `glGetActiveUniformsiv(GL_UNIFORM_MATRIX_STRIDE)` must report, and it lives beside
    /// [`Self::layout`] because that is where the rule already is: a matrix is an array of column vectors
    /// each padded to sixteen bytes, and the sizes above are derived from exactly that. Reporting it from
    /// a second copy of the constant is how the two drift.
    pub(super) fn std140_matrix_stride(&self) -> i32 {
        match self.0 {
            "mat2" | "mat2x2" | "mat3" | "mat3x3" | "mat4" | "mat4x4" | "mat2x3" | "mat2x4"
            | "mat3x2" | "mat3x4" | "mat4x2" | "mat4x3" => MATRIX_COLUMN_STRIDE,
            _ => 0,
        }
    }
}

/// std140 pads every matrix column to a `vec4`. The matrix sizes in [`TypeToken::layout`] are this times
/// the column count.
pub(super) const MATRIX_COLUMN_STRIDE: i32 = 16;

// Uniform layout and block reflection continue in `uniforms`.
