//! GLSL-ES front-end — reflection + a GLSL-ES → naga-acceptable *desktop* GLSL rewrite.
//!
//! The host owns the shader compiler (naga's `glsl-in` on the wgpu executor), so the guest driver FORWARDS
//! GLSL source rather than pre-translating to a backend IR. naga's `glsl-in` accepts only DESKTOP GLSL
//! (`#version 440+`, `layout`-qualified `in`/`out`, explicit fragment outputs, `layout(binding=)` uniform
//! blocks) — not the GLES `attribute`/`varying`/`gl_FragColor`/`#version N es` dialect — so
//! [`translate_render`] regenerates each stage's DECLARATIONS into the desktop form from the reflected
//! interface and carries the shader BODY through (desktop GLSL is a superset of the ES body syntax). Each
//! stage is packed into its own `GlslDescriptor` (`ShaderPayloadKind::Glsl`) at `glLinkProgram`. The public
//! reflection helpers ([`collect_vertex_attrs`], [`uni_layout`], [`program_samplers`]) feed the pipeline's
//! vertex layout + the uniform/sampler bind-group emission at swap.

/// A parsed `qualifier TYPE name[arr];` declaration (gl_shim.c `struct decl`). `arr` is the array element
/// count (`0` = not an array) — Skia declares default-block uniforms as arrays (`uniform vec4 uKernel[8];`)
/// which the emitted `HlUniforms` block and its std140 layout must preserve, or naga sees a scalar indexed
/// like an array and rejects the store type.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Decl {
    pub ty: String,
    pub name: String,
    pub arr: u32,
}

impl Decl {
    fn is_sampler(&self) -> bool {
        TypeToken(&self.ty).is_sampler()
    }

    fn requires_flat_interpolation(&self) -> bool {
        matches!(
            self.ty.as_str(),
            "int"
                | "uint"
                | "bool"
                | "ivec2"
                | "ivec3"
                | "ivec4"
                | "uvec2"
                | "uvec3"
                | "uvec4"
                | "bvec2"
                | "bvec3"
                | "bvec4"
        )
    }
}

struct TypeToken<'a>(&'a str);

impl TypeToken<'_> {
    fn is_sampler(&self) -> bool {
        matches!(
            self.0,
            "sampler2D"
                | "samplerCube"
                | "sampler2DArray"
                | "sampler2DShadow"
                | "samplerExternalOES"
        )
    }

    fn is_regenerated_qualifier(&self) -> bool {
        matches!(
            self.0,
            "attribute"
                | "varying"
                | "uniform"
                | "precision"
                | "const"
                | "in"
                | "out"
                | "flat"
                | "smooth"
                | "centroid"
                | "invariant"
                | "layout"
        )
    }

    fn is_precision(&self) -> bool {
        matches!(self.0, "highp" | "mediump" | "lowp")
    }

    fn is_io_qualifier(&self) -> bool {
        matches!(
            self.0,
            "flat"
                | "smooth"
                | "noperspective"
                | "centroid"
                | "sample"
                | "invariant"
                | "precise"
                | "highp"
                | "mediump"
                | "lowp"
        )
    }
}

/// A uniform-block member's byte offset/size (gl_shim.c `struct uni`).
#[derive(Clone, Debug, PartialEq)]
pub struct Uni {
    pub name: String,
    pub off: i32,
    pub sz: i32,
}

/// One GLSL stage's source text.  Scanning and source-preserving rewrites live here so callers cannot
/// accidentally mix comment-stripped offsets with the original byte stream.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    text: &'a str,
}

pub struct StageSources<'a> {
    vertex: &'a str,
    fragment: &'a str,
}

pub struct Translator;

impl<'a> StageSources<'a> {
    pub fn new(vertex: &'a str, fragment: &'a str) -> Self {
        Self { vertex, fragment }
    }
}

impl<'a> Source<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub fn vertex_attrs(self) -> Vec<Decl> {
        let text = self.comments_removed();
        let mut attrs = Tokens(&text).collect("attribute");
        attrs.truncate(16);
        append_decls_unique(&mut attrs, Tokens(&text).collect("in"), 16);
        attrs
    }

    pub fn inject_uniform_block_bindings(self) -> String {
        UniformBlockEdits::new(self.text).apply()
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL-ES compute → naga-acceptable desktop GLSL compute (the CreateShader Glsl payload)

mod bindings;
mod locations;
mod reflection;
mod scanner;
mod tokens;
mod translate;
mod uniforms;

pub use bindings::prepare_verbatim_program;
pub use uniforms::UniformBlockDecl;

use bindings::UniformBlockEdits;
use scanner::*;
use tokens::*;
