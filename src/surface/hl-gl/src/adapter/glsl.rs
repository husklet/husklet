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
    pub array_literal: bool,
}

impl Decl {
    /// How many consecutive interface locations this declaration occupies: one per matrix COLUMN, times
    /// the array length. A `mat4` takes four and `vec4 c[2]` takes two, so numbering declarations
    /// sequentially overlaps them — the program then links, draws with `GL_NO_ERROR` and paints nothing.
    pub(crate) fn location_span(&self) -> u32 {
        Declarations::location_span(&self.ty, self.arr)
    }

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
        ) || self.is_integer_sampler()
    }

    /// The INTEGER sampler types (`usampler2D` / `isampler2D` and their array forms), which read a texture
    /// of raw integer texels.
    ///
    /// These were absent from `is_sampler`, so an integer sampler was classified as a DATA uniform: it was
    /// emitted into the uniform block, no texture binding was made for it, and the shader failed to
    /// compile. That is why integer textures "sampled as zero" — nothing was sampling at all.
    fn is_integer_sampler(&self) -> bool {
        matches!(
            self.0,
            "usampler2D" | "isampler2D" | "usampler2DArray" | "isampler2DArray"
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
    pub ty: String,
    pub arr: u32,
}

impl Uni {
    /// Copy packed `glUniform*` input into this member's std140 storage, inserting array/matrix column
    /// padding rather than treating the caller's tightly packed bytes as already-std140.
    pub fn write(&self, block: &mut [u8], bytes: &[u8]) {
        self.write_from(block, 0, bytes);
    }

    pub fn write_from(&self, block: &mut [u8], first_element: usize, bytes: &[u8]) {
        let offset = self.off.max(0) as usize;
        let size = self.sz.max(0) as usize;
        let Some(target) = block.get_mut(offset..offset.saturating_add(size)) else {
            return;
        };
        let elements = if self.arr == 0 { 1 } else { self.arr as usize };
        if first_element >= elements {
            return;
        }
        let element_stride = size / elements.max(1);
        if let Some((columns, rows)) = matrix_shape(&self.ty) {
            let packed_element = columns * rows * 4;
            for element in first_element..elements {
                let source_base = (element - first_element) * packed_element;
                let target_base = element * element_stride;
                for column in 0..columns {
                    let source = source_base + column * rows * 4;
                    let target_offset = target_base + column * 16;
                    let count = rows * 4;
                    let (Some(input), Some(output)) = (
                        bytes.get(source..source + count),
                        target.get_mut(target_offset..target_offset + count),
                    ) else {
                        return;
                    };
                    output.copy_from_slice(input);
                }
            }
            return;
        }

        let packed_element = TypeToken(&self.ty)
            .layout()
            .map(|(_, _, components)| components * 4)
            .unwrap_or(0);
        for element in first_element..elements {
            let source = (element - first_element) * packed_element;
            let target_offset = element * element_stride;
            let count = packed_element
                .min(bytes.len().saturating_sub(source))
                .min(target.len().saturating_sub(target_offset));
            if count == 0 {
                break;
            }
            target[target_offset..target_offset + count]
                .copy_from_slice(&bytes[source..source + count]);
        }
    }

    pub fn read_element(&self, block: &[u8], element: usize) -> Option<Vec<u8>> {
        let offset = self.off.max(0) as usize;
        let size = self.sz.max(0) as usize;
        let source = block.get(offset..offset.checked_add(size)?)?;
        let elements = self.arr.max(1) as usize;
        if element >= elements {
            return None;
        }
        let stride = size / elements;
        let source = source.get(element * stride..(element + 1) * stride)?;
        if let Some((columns, rows)) = matrix_shape(&self.ty) {
            let mut packed = Vec::with_capacity(columns * rows * 4);
            for column in 0..columns {
                packed.extend_from_slice(source.get(column * 16..column * 16 + rows * 4)?);
            }
            return Some(packed);
        }
        let bytes = TypeToken(&self.ty).layout()?.2 * 4;
        Some(source.get(..bytes)?.to_vec())
    }
}

fn matrix_shape(ty: &str) -> Option<(usize, usize)> {
    Some(match ty {
        "mat2" | "mat2x2" => (2, 2),
        "mat3" | "mat3x3" => (3, 3),
        "mat4" | "mat4x4" => (4, 4),
        "mat2x3" => (2, 3),
        "mat2x4" => (2, 4),
        "mat3x2" => (3, 2),
        "mat3x4" => (3, 4),
        "mat4x2" => (4, 2),
        "mat4x3" => (4, 3),
        _ => return None,
    })
}

/// Why a GLSL program cannot be represented by the modeled GLES uniform interface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UniformError {
    /// The stage could not be preprocessed (GLSL ES 1.00 §3.4); nothing downstream ran.
    Preprocess(PreprocessError),
    UnsupportedType(String),
    NonLiteralArray(String),
    DynamicSamplerArray(String),
    ArithmeticOverflow,
    StageComponents {
        stage: &'static str,
        count: usize,
    },
    BlockBytes(usize),
    Samplers(usize),
    ConflictingDeclaration(String),
    AttributeLocation(String),
    TransformFeedback(String),
    /// A compute shader declared a DEFAULT-BLOCK uniform. The compute path binds only the
    /// `glBindBufferBase`d UBO/SSBO bindings, so such a uniform would silently read zero — hence the
    /// advertised `GL_MAX_COMPUTE_UNIFORM_COMPONENTS = 0` and this loud refusal at link.
    ComputeDefaultBlock(String),
    /// A stage has no `main` whose body can be found and closed — no `main(` at all, or an opening brace
    /// that never closes. The translator regenerates a stage from its reflected declarations plus this
    /// body, so without this refusal a shader with a dropped brace became `void main() {}`: the host
    /// front end accepted it, the pipeline built, the draw ran, nothing was written, and no layer
    /// reported a thing. A wrong render with a clean status is worse than a refused one.
    MainBody {
        stage: &'static str,
    },
}

impl std::fmt::Display for UniformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preprocess(error) => write!(f, "preprocessor error at line {error}"),
            Self::UnsupportedType(ty) => write!(f, "unsupported uniform type `{ty}`"),
            Self::NonLiteralArray(name) => {
                write!(f, "uniform `{name}` has a non-literal array dimension")
            }
            Self::DynamicSamplerArray(name) => {
                write!(f, "sampler array `{name}` uses a non-constant index")
            }
            Self::ArithmeticOverflow => write!(f, "uniform layout arithmetic overflow"),
            Self::StageComponents { stage, count } => write!(
                f,
                "{stage} shader uses {count} uniform components; the limit is {MAX_UNIFORM_COMPONENTS}"
            ),
            Self::BlockBytes(bytes) => write!(
                f,
                "combined uniform block uses {bytes} bytes; the limit is {MAX_COMBINED_UNIFORM_BYTES}"
            ),
            Self::Samplers(count) => write!(
                f,
                "shader/program uses {count} samplers (counting array elements); the supported limit was exceeded"
            ),
            Self::ConflictingDeclaration(name) => {
                write!(f, "uniform `{name}` has conflicting stage declarations")
            }
            Self::AttributeLocation(name) => {
                write!(
                    f,
                    "attribute `{name}` has a conflicting or invalid location"
                )
            }
            Self::TransformFeedback(reason) => write!(f, "transform feedback: {reason}"),
            Self::MainBody { stage } => write!(
                f,
                "{stage} shader has no complete `void main()` body — check for an unclosed brace"
            ),
            Self::ComputeDefaultBlock(name) => write!(
                f,
                "compute shader declares default-block uniform `{name}`; \
                 GL_MAX_COMPUTE_UNIFORM_COMPONENTS is 0 — use a uniform block bound with glBindBufferBase"
            ),
        }
    }
}

impl std::error::Error for UniformError {}

impl From<PreprocessError> for UniformError {
    fn from(error: PreprocessError) -> Self {
        Self::Preprocess(error)
    }
}

/// The per-stage default-block ceiling ENFORCED here, kept identical to the advertised
/// `GL_MAX_VERTEX_UNIFORM_COMPONENTS` / `GL_MAX_FRAGMENT_UNIFORM_COMPONENTS` (see
/// [`crate::service::query::MAX_UNIFORM_COMPONENTS`] for what backs the number). The two must never
/// drift: an advertised limit the linker refuses is exactly the over-report this table exists to avoid.
pub const MAX_UNIFORM_COMPONENTS: usize = crate::service::query::MAX_UNIFORM_COMPONENTS as usize;
/// Per-stage sampler ceiling: the GLES3 minimum (16) and simultaneously wgpu's guaranteed
/// 16 sampled-textures + 16 samplers per shader stage, which is Metal's per-stage sampler-argument
/// limit. A program declaring more is rejected at link.
pub const MAX_VERTEX_SAMPLERS: usize = 16;
pub const MAX_FRAGMENT_SAMPLERS: usize = 16;
/// Both stages together, bounded by the modelled texture-unit bank.
pub const MAX_COMBINED_SAMPLERS: usize = crate::model::glconst::MAX_TEXTURE_UNITS;
/// Two independently valid stages are flattened into one internal std140 block. A scalar array is the
/// worst-case std140 expansion: each component occupies one 16-byte array stride.
pub const MAX_COMBINED_UNIFORM_BYTES: usize = 2 * MAX_UNIFORM_COMPONENTS * 16;

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
        let text = self.expanded();
        let mut attrs = Tokens(&text).collect("attribute");
        attrs.truncate(16);
        append_decls_unique(&mut attrs, Tokens(&text).collect("in"), 16);
        attrs
    }

    /// The preprocessed stage, or — when preprocessing fails — the merely comment-free source.
    ///
    /// Every route to the host compiler is gated by [`StageSources::validate_sampler_array_uses`] and
    /// [`StageSources::uniform_layout`], both of which REJECT the program with the preprocessor diagnostic
    /// before any translation runs, so this fallback only ever feeds reflection whose result is discarded
    /// together with the failed link. Reflection helpers stay infallible for their callers.
    pub(super) fn expanded(self) -> String {
        self.preprocessed()
            .unwrap_or_else(|_| self.comments_removed())
    }

    pub fn inject_uniform_block_bindings(self) -> String {
        UniformBlockEdits::new(self.text).apply()
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL-ES compute → naga-acceptable desktop GLSL compute (the CreateShader Glsl payload)

mod bindings;
mod constants;
mod locations;
mod normalize;
mod preprocess;
mod reflection;
mod scanner;
mod tokens;
mod translate;
mod uniforms;
mod validation;

pub use validation::invalid_implicit_arithmetic;

pub use bindings::{prepare_verbatim_program, prepare_verbatim_program_with};
pub use preprocess::PreprocessError;
pub use uniforms::{compute_default_block_uniform, std140_array_stride, UniformBlockDecl};

/// The std140 column stride `glGetActiveUniformsiv(GL_UNIFORM_MATRIX_STRIDE)` reports for a GLSL type
/// keyword, or `0` when the type is not a matrix. Derived from the same rule that lays the block out.
pub fn std140_matrix_stride(ty: &str) -> i32 {
    TypeToken(ty).std140_matrix_stride()
}

use bindings::UniformBlockEdits;
use constants::Constants;
use normalize::NormalizedSource;
use scanner::*;
use tokens::*;

/// The GLSL-ES version a shader declares, as the `#version` integer (`300` for `#version 300 es`).
///
/// `None` when the shader declares no `#version` at all, which GLSL-ES defines as version 100.
pub fn declared_es_version(source: &str) -> u32 {
    for line in source.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("#version") else {
            continue;
        };
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        return digits.parse().unwrap_or(100);
    }
    100
}

/// Built-ins that GLSL-ES introduced at 3.10 and which a 3.00 or 1.00 shader may not use.
///
/// A shader that uses one under `#version 300 es` compiles here and fails on conformant hardware, which
/// is the worst way for an author to find out. Each entry is a FUNCTION, matched only as a call, so a
/// user-defined variable of the same name is untouched.
///
/// Deliberately confined to the unambiguous 3.10 additions of GLSL-ES §8 — the integer/bit-manipulation
/// group, the extended-arithmetic group, `frexp`/`ldexp`, `textureGather`, and the image/atomic entry
/// points. Constructs that merely *look* newer are excluded: `packSnorm2x16` and friends are 3.00, and
/// `texelFetch` is 3.00.
const ES_310_BUILTINS: &[&str] = &[
    "bitCount",
    "bitfieldExtract",
    "bitfieldInsert",
    "bitfieldReverse",
    "findLSB",
    "findMSB",
    "frexp",
    "ldexp",
    "imulExtended",
    "umulExtended",
    "uaddCarry",
    "usubBorrow",
    "textureGather",
    "textureGatherOffset",
    "imageLoad",
    "imageStore",
    "imageSize",
    "atomicAdd",
    "atomicMin",
    "atomicMax",
    "atomicAnd",
    "atomicOr",
    "atomicXor",
    "atomicExchange",
    "atomicCompSwap",
];

/// The first ES 3.10 built-in this source calls, when its declared version is below 3.10.
///
/// Returns `None` for a shader that is within its declared version — including every 3.10-or-later
/// shader, which may use all of them.
pub fn builtin_above_declared_version(source: &str) -> Option<&'static str> {
    if declared_es_version(source) >= 310 {
        return None;
    }
    let bytes = source.as_bytes();
    for name in ES_310_BUILTINS {
        let mut from = 0usize;
        while let Some(offset) = source[from..].find(name) {
            let start = from + offset;
            let end = start + name.len();
            let before_is_word = start > 0 && Tokens::is_word(bytes[start - 1]);
            // A CALL: the next non-space character is `(`. A declaration or a member access is not one.
            let after = source[end..].trim_start();
            let is_call = after.starts_with('(');
            let is_member = start > 0 && bytes[start - 1] == b'.';
            if !before_is_word && !is_member && is_call {
                return Some(name);
            }
            from = end;
        }
    }
    None
}
