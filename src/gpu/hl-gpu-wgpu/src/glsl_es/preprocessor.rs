use super::io::rewrite::replace_ident;

pub(super) struct Preprocessor<'a>(pub(super) &'a str);

impl Preprocessor<'_> {
    /// Rewrites GskGpu's `IN`, `PASS`, and `PASS_FLAT` definitions to retain their explicit location.
    pub(in crate::glsl_es) fn io_macro(&self) -> Option<String> {
        let rest = self
            .0
            .trim_start()
            .strip_prefix('#')?
            .trim_start()
            .strip_prefix("define")?;
        if !rest.starts_with(char::is_whitespace) {
            return None; // `#defineX` — not a define directive
        }
        let rest = rest.trim_start();
        let paren = rest.find('(')?;
        let name = rest[..paren].trim();
        if !matches!(name, "IN" | "PASS" | "PASS_FLAT") {
            return None;
        }
        let after = &rest[paren + 1..];
        let close = after.find(')')?;
        let param = after[..close].trim();
        if param.is_empty() {
            return None;
        }
        let body = after[close + 1..].trim();
        if !matches!(body, "in" | "out" | "flat in" | "flat out") {
            return None;
        }
        Some(format!(
            "#define {name}({param}) layout(location = {param}) {body}"
        ))
    }

    pub(in crate::glsl_es) fn vertex_builtins(&self) -> String {
        let source = replace_ident(self.0, "gl_VertexID", "int(gl_VertexIndex)");
        replace_ident(&source, "gl_InstanceID", "int(gl_InstanceIndex)")
    }
}

// ---------------------------------------------------------------------------------------------------
// Aggregate vertex-input / varying splitting
// ---------------------------------------------------------------------------------------------------
//
// naga requires every vertex `in`/`out` and inter-stage varying to be an *IO-shareable* type — a numeric
// scalar or vector (or a struct of such with per-member `@location`s). A matrix or an array as a *single*
// located interface member fails validation with `Argument(n, NotIOShareableType)`. GskGpu emits both:
//
//   IN(0) mat3x4 in_outline;               // a matrix vertex attribute (3 vec4 columns)
//   PASS_FLAT(2) RoundedRect _outline;     // RoundedRect == `vec4[3]`, an array varying
//
// In real desktop GL a `matCxR`/array attribute silently consumes C (or N) consecutive locations; GskGpu's
// own `_loc` numbering already leaves that room (the next input after `IN(0) mat3x4` is `IN(3)`). So we
// split each aggregate interface member into its C/N per-location vector slots (`name_hlio0…`), keep a
// private (non-interface) global of the original aggregate type so every *use* site is unchanged, and
// bridge the two at the entry point: for an input, reconstruct the global from the slots at the top of
// `main`; for an output, scatter the global into the slots at the end of `main`. No data the fragment
// stage needs is dropped — the aggregate is carried in full across the (now IO-shareable) vector slots.
//
// The generated declarations are HOISTED to just after `#version`, not left at the original declaration
// site: GskGpu's `main` (from the shared common.glsl) is emitted *before* the per-op I/O declarations, and
// the entry-point bridge we inject into `main` would otherwise reference globals GLSL has not seen yet.
