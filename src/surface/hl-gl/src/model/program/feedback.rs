//! Linked transform-feedback capture layout.

use crate::adapter::glsl::{Decl, StageSources};
use crate::model::glconst::{GL_INTERLEAVED_ATTRIBS, GL_SEPARATE_ATTRIBS};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureScalarKind {
    Float,
    Sint,
    Uint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureScalar {
    pub expression: String,
    pub kind: CaptureScalarKind,
    pub buffer: u32,
    pub word: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransformFeedbackVarying {
    pub name: String,
    pub size: i32,
    pub gl_type: u32,
    pub buffer: u32,
    pub words: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransformFeedbackLayout {
    pub mode: u32,
    pub buffers: u32,
    pub strides: [u32; 4],
    pub varyings: Vec<TransformFeedbackVarying>,
    pub scalars: Vec<CaptureScalar>,
}

#[derive(Clone, Copy)]
struct Shape {
    kind: CaptureScalarKind,
    columns: u32,
    rows: u32,
    gl_type: u32,
}

impl Shape {
    fn of(name: &str) -> Option<Self> {
        use CaptureScalarKind::{Float, Sint, Uint};
        let (kind, columns, rows, gl_type) = match name {
            "float" => (Float, 1, 1, 0x1406),
            "vec2" => (Float, 1, 2, 0x8B50),
            "vec3" => (Float, 1, 3, 0x8B51),
            "vec4" => (Float, 1, 4, 0x8B52),
            "int" => (Sint, 1, 1, 0x1404),
            "ivec2" => (Sint, 1, 2, 0x8B53),
            "ivec3" => (Sint, 1, 3, 0x8B54),
            "ivec4" => (Sint, 1, 4, 0x8B55),
            "uint" => (Uint, 1, 1, 0x1405),
            "uvec2" => (Uint, 1, 2, 0x8DC6),
            "uvec3" => (Uint, 1, 3, 0x8DC7),
            "uvec4" => (Uint, 1, 4, 0x8DC8),
            "mat2" | "mat2x2" => (Float, 2, 2, 0x8B5A),
            "mat3" | "mat3x3" => (Float, 3, 3, 0x8B5B),
            "mat4" | "mat4x4" => (Float, 4, 4, 0x8B5C),
            "mat2x3" => (Float, 2, 3, 0x8B65),
            "mat2x4" => (Float, 2, 4, 0x8B66),
            "mat3x2" => (Float, 3, 2, 0x8B67),
            "mat3x4" => (Float, 3, 4, 0x8B68),
            "mat4x2" => (Float, 4, 2, 0x8B69),
            "mat4x3" => (Float, 4, 3, 0x8B6A),
            _ => return None,
        };
        Some(Self {
            kind,
            columns,
            rows,
            gl_type,
        })
    }

    fn words(self) -> u32 {
        self.columns * self.rows
    }
}

impl TransformFeedbackLayout {
    pub fn reflect(source: &str, names: &[String], mode: u32) -> Result<Self, String> {
        if !matches!(mode, GL_INTERLEAVED_ATTRIBS | GL_SEPARATE_ATTRIBS) {
            return Err("invalid transform-feedback buffer mode".into());
        }
        if mode == GL_SEPARATE_ATTRIBS && names.len() > 4 {
            return Err("too many separate transform-feedback varyings".into());
        }
        let mut outputs = StageSources::new(source, "").vertex_outputs();
        outputs.push(Decl {
            ty: "vec4".into(),
            name: "gl_Position".into(),
            arr: 0,
            array_literal: false,
        });
        outputs.push(Decl {
            ty: "float".into(),
            name: "gl_PointSize".into(),
            arr: 0,
            array_literal: false,
        });
        let mut layout = Self {
            mode,
            buffers: if mode == GL_SEPARATE_ATTRIBS {
                names.len() as u32
            } else {
                (!names.is_empty()) as u32
            },
            strides: [0; 4],
            varyings: Vec::with_capacity(names.len()),
            scalars: Vec::new(),
        };
        for (varying_index, requested) in names.iter().enumerate() {
            let (decl, expression, elements) = resolve(&outputs, requested)?;
            let shape = Shape::of(&decl.ty)
                .ok_or_else(|| format!("unsupported transform-feedback type {}", decl.ty))?;
            let buffer = if mode == GL_SEPARATE_ATTRIBS {
                varying_index as u32
            } else {
                0
            };
            let words = shape
                .words()
                .checked_mul(elements)
                .ok_or_else(|| "transform-feedback varying size overflow".to_string())?;
            let base_word = layout.strides[buffer as usize] / 4;
            for element in 0..elements {
                let element_expression = if elements > 1 || decl.arr > 1 {
                    format!("{expression}[{element}]")
                } else {
                    expression.clone()
                };
                for column in 0..shape.columns {
                    for row in 0..shape.rows {
                        let expression = match (shape.columns, shape.rows) {
                            (1, 1) => element_expression.clone(),
                            (1, _) => format!("{element_expression}[{row}]"),
                            _ => format!("{element_expression}[{column}][{row}]"),
                        };
                        layout.scalars.push(CaptureScalar {
                            expression,
                            kind: shape.kind,
                            buffer,
                            word: base_word + element * shape.words() + column * shape.rows + row,
                        });
                    }
                }
            }
            layout.strides[buffer as usize] = layout.strides[buffer as usize]
                .checked_add(words * 4)
                .ok_or_else(|| "transform-feedback stride overflow".to_string())?;
            layout.varyings.push(TransformFeedbackVarying {
                name: requested.clone(),
                size: elements as i32,
                gl_type: shape.gl_type,
                buffer,
                words,
            });
        }
        Ok(layout)
    }

    /// Wrap the real translated vertex entry point with deterministic raw-word stores. The per-invocation
    /// destination base is supplied by a small uniform; every logical invocation gets its own base, so no
    /// result depends on vertex scheduling or atomics.
    pub fn capture_source(&self, source: &str) -> Result<String, String> {
        let marker = "void main";
        let at = source
            .find(marker)
            .ok_or_else(|| "translated vertex shader has no main".to_string())?;
        let name = at + "void ".len();
        let mut wrapped = source.to_string();
        wrapped.replace_range(name..name + "main".len(), "hl_tf_user_main");
        wrapped.push_str("\n");
        for buffer in 0..self.buffers {
            wrapped.push_str(&format!(
                "layout(set=0, binding={}, std430) buffer HlTfBuffer{buffer} {{ uint words[]; }} hl_tf{buffer};\n",
                64 + buffer
            ));
        }
        wrapped.push_str(
            "layout(set=0, binding=68, std140) uniform HlTfOffsets { uvec4 base_words; } hl_tf_offsets;\n\
             void main() {\n  hl_tf_user_main();\n",
        );
        for scalar in &self.scalars {
            let component = match scalar.buffer {
                0 => "x",
                1 => "y",
                2 => "z",
                _ => "w",
            };
            let value = match scalar.kind {
                CaptureScalarKind::Float => format!("floatBitsToUint({})", scalar.expression),
                CaptureScalarKind::Sint | CaptureScalarKind::Uint => {
                    format!("uint({})", scalar.expression)
                }
            };
            wrapped.push_str(&format!(
                "  hl_tf{}.words[hl_tf_offsets.base_words.{component} + {}u] = {value};\n",
                scalar.buffer, scalar.word
            ));
        }
        wrapped.push_str("}\n");
        Ok(wrapped)
    }
}

fn resolve<'a>(outputs: &'a [Decl], requested: &str) -> Result<(&'a Decl, String, u32), String> {
    let (base, selected) = requested
        .strip_suffix(']')
        .and_then(|name| name.rsplit_once('['))
        .and_then(|(base, index)| index.parse::<u32>().ok().map(|index| (base, index)))
        .map_or((requested, None), |(base, index)| (base, Some(index)));
    let declaration = outputs
        .iter()
        .find(|declaration| declaration.name == base)
        .ok_or_else(|| format!("unknown transform-feedback varying {requested}"))?;
    let array = declaration.arr.max(1);
    if let Some(index) = selected {
        if index >= array {
            return Err(format!(
                "transform-feedback array index out of range: {requested}"
            ));
        }
        return Ok((declaration, requested.into(), 1));
    }
    Ok((declaration, requested.into(), array))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_size_is_one_float_word() {
        let layout = TransformFeedbackLayout::reflect(
            "#version 300 es\nin vec4 p; void main(){ gl_Position=p; gl_PointSize=p.x; }",
            &["gl_PointSize".into()],
            0x8C8C,
        )
        .unwrap();
        assert_eq!(layout.buffers, 1);
        assert_eq!(layout.strides, [4, 0, 0, 0]);
        assert_eq!(layout.scalars[0].expression, "gl_PointSize");
        assert_eq!(layout.scalars[0].word, 0);
        assert_eq!(layout.varyings[0].gl_type, 0x1406);
    }

    #[test]
    fn matrices_and_arrays_expand_in_column_major_order() {
        let source = "#version 300 es\nout mat2x3 m[2]; void main(){}";
        let layout = TransformFeedbackLayout::reflect(source, &["m".into()], 0x8C8C).unwrap();
        assert_eq!(layout.strides[0], 48);
        assert_eq!(layout.scalars.len(), 12);
        assert_eq!(layout.scalars[0].expression, "m[0][0][0]");
        assert_eq!(layout.scalars[5].expression, "m[0][1][2]");
        assert_eq!(layout.scalars[6].expression, "m[1][0][0]");
        assert_eq!(layout.varyings[0].size, 2);
        assert_eq!(layout.varyings[0].gl_type, 0x8B65);
    }

    #[test]
    fn separate_mode_assigns_one_buffer_and_stride_per_varying() {
        let source = "#version 300 es\nout vec2 a; flat out uvec3 b; void main(){}";
        let layout =
            TransformFeedbackLayout::reflect(source, &["a".into(), "b".into()], 0x8C8D).unwrap();
        assert_eq!(layout.buffers, 2);
        assert_eq!(layout.strides, [8, 12, 0, 0]);
        assert_eq!(layout.scalars[2].buffer, 1);
        assert_eq!(layout.scalars[2].word, 0);
        assert_eq!(layout.scalars[2].kind, CaptureScalarKind::Uint);
    }

    #[test]
    fn capture_source_executes_real_shader_then_stores_actual_output() {
        let source = "#version 460\nlayout(location=0) in float a; void main(){ gl_PointSize=a; }";
        let layout =
            TransformFeedbackLayout::reflect(source, &["gl_PointSize".into()], 0x8C8C).unwrap();
        let capture = layout.capture_source(source).unwrap();
        assert!(capture.contains("void hl_tf_user_main()"));
        assert!(capture.contains("hl_tf_user_main();"));
        assert!(capture.contains("floatBitsToUint(gl_PointSize)"));
        assert!(capture.contains("set=0, binding=64"));
        assert!(!capture.contains("gl_PointSize = 1.0"));
    }

    #[test]
    fn deqp_point_size_capture_keeps_input_assignment() {
        let source = "#version 300 es\nin highp vec4 a_position;\nin highp float a_pointSize;\nvoid main(void){ gl_Position=a_position; gl_PointSize=a_pointSize; }";
        let (translated, _) = StageSources::new(
            source,
            "#version 300 es\nprecision mediump float; out vec4 c; void main(){c=vec4(0.0);}",
        )
        .translate_render();
        let layout =
            TransformFeedbackLayout::reflect(source, &["gl_PointSize".into()], 0x8C8C).unwrap();
        let capture = layout.capture_source(&translated).unwrap();
        assert!(capture.contains("gl_PointSize=a_pointSize"), "{capture}");
        assert!(
            capture.contains("floatBitsToUint(gl_PointSize)"),
            "{capture}"
        );
    }
}
