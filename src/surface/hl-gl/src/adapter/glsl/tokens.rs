use super::*;

pub(super) struct Tokens<'a>(pub(super) &'a str);

impl<'a> Tokens<'a> {
    pub(super) fn first_word(&self) -> &'a str {
        let u = self.0;
        u.split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
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
/// (gl_shim.c `collect_uniforms`). Returns `(data, samplers)` capped at 16 / 4.
pub(super) struct Declarations<'a> {
    vertex: &'a str,
    fragment: &'a str,
}

impl<'a> Declarations<'a> {
    pub(super) fn from_stages(vertex: &'a str, fragment: &'a str) -> Self {
        Self { vertex, fragment }
    }

    pub(super) fn uniforms(self) -> (Vec<Decl>, Vec<Decl>) {
        let mut all = Tokens(self.vertex).collect("uniform");
        all.truncate(32);
        for d in Tokens(self.fragment).collect("uniform") {
            if all.len() >= 32 {
                break;
            }
            if !all.iter().any(|x| x.name == d.name) {
                all.push(d);
            }
        }
        let (mut data, mut samps) = (Vec::new(), Vec::new());
        for d in all {
            if d.is_sampler() {
                if samps.len() < 4 {
                    samps.push(d);
                }
            } else if data.len() < 16 {
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
    pub(super) fn layout(&self) -> Option<(i32, i32)> {
        Some(match self.0 {
            "float" | "int" | "uint" | "bool" => (4, 4),
            "vec2" | "ivec2" | "uvec2" | "bvec2" => (8, 8),
            "vec3" | "ivec3" | "uvec3" | "bvec3" => (16, 16),
            "vec4" | "ivec4" | "uvec4" | "bvec4" => (16, 16),
            "mat2" | "mat2x2" => (16, 8),
            "mat3" | "mat3x3" => (48, 16),
            "mat4" | "mat4x4" => (64, 16),
            "mat2x3" => (32, 16),
            "mat2x4" => (32, 16),
            "mat3x2" => (24, 8),
            "mat3x4" => (48, 16),
            "mat4x2" => (32, 8),
            "mat4x3" => (64, 16),
            _ => return None,
        })
    }
}

// Uniform layout and block reflection continue in `uniforms`.
