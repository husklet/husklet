use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{GpuError, Result};
use naga::{Expression, Handle, Span, Type};

pub(super) fn format_shape(format: TextureFormat) -> Result<(naga::ScalarKind, u32)> {
    use naga::ScalarKind::{Float, Sint, Uint};
    let shape = match format {
        TextureFormat::R8Unorm | TextureFormat::R8Uint | TextureFormat::R8Sint =>
            (if format == TextureFormat::R8Uint { Uint } else if format == TextureFormat::R8Sint { Sint } else { Float }, 1),
        TextureFormat::Rg8Unorm | TextureFormat::Rg8Snorm | TextureFormat::Rg8Uint | TextureFormat::Rg8Sint =>
            (if format == TextureFormat::Rg8Uint { Uint } else if format == TextureFormat::Rg8Sint { Sint } else { Float }, 2),
        TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Snorm => (Float, 4),
        TextureFormat::Rg16Float => (Float, 4),
        TextureFormat::R16Float | TextureFormat::R16Uint | TextureFormat::R16Sint =>
            (if format == TextureFormat::R16Uint { Uint } else if format == TextureFormat::R16Sint { Sint } else { Float }, 2),
        TextureFormat::Rg16Uint | TextureFormat::Rg16Sint => (if format == TextureFormat::Rg16Uint { Uint } else { Sint }, 4),
        TextureFormat::Rgba16Uint | TextureFormat::Rgba16Sint => (if format == TextureFormat::Rgba16Uint { Uint } else { Sint }, 8),
        TextureFormat::Rg32Float | TextureFormat::Rg32Uint | TextureFormat::Rg32Sint =>
            (if format == TextureFormat::Rg32Uint { Uint } else if format == TextureFormat::Rg32Sint { Sint } else { Float }, 8),
        TextureFormat::Rgba16Float => (Float, 8),
        TextureFormat::R32Float => (Float, 4),
        TextureFormat::Rgba32Float => (Float, 16),
        TextureFormat::Rgba8Uint | TextureFormat::R32Uint => (Uint, 4),
        TextureFormat::Rgba32Uint => (Uint, 16),
        TextureFormat::Rgba8Sint | TextureFormat::R32Sint => (Sint, 4),
        TextureFormat::Rgba32Sint => (Sint, 16),
        _ => {
            return Err(GpuError::Unsupported(
                "wgpu: packed texel format needs raw specialization",
            ))
        }
    };
    Ok(shape)
}

pub(super) fn decode(
    function: &mut naga::Function,
    format: TextureFormat,
    vec4: Handle<Type>,
    index: Handle<Expression>,
    words: &[Handle<Expression>],
) -> Result<Handle<Expression>> {
    let value = match format {
        TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Snorm => function.expressions.append(
            Expression::Math {
                fun: if format == TextureFormat::Rgba8Snorm { naga::MathFunction::Unpack4x8snorm } else { naga::MathFunction::Unpack4x8unorm },
                arg: words[0],
                arg1: None,
                arg2: None,
                arg3: None,
            },
            Span::default(),
        ),
        TextureFormat::R8Unorm | TextureFormat::Rg8Unorm | TextureFormat::Rg8Snorm => {
            let count = if format == TextureFormat::R8Unorm { 1 } else { 2 };
            let mut components = (0..count)
                .map(|component| normalized_byte(function, words[0], index, count, component, format == TextureFormat::Rg8Snorm))
                .collect::<Vec<_>>();
            components.push(scalar_literal(function, naga::ScalarKind::Float, 0));
            if count == 1 {
                components.push(scalar_literal(function, naga::ScalarKind::Float, 0));
            }
            components.push(scalar_literal(function, naga::ScalarKind::Float, 1));
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::R8Uint | TextureFormat::Rg8Uint | TextureFormat::R8Sint | TextureFormat::Rg8Sint => {
            let count = if matches!(format, TextureFormat::R8Uint | TextureFormat::R8Sint) { 1 } else { 2 };
            let signed = matches!(format, TextureFormat::R8Sint | TextureFormat::Rg8Sint);
            let mut components = (0..count)
                .map(|component| packed_byte_component(function, words[0], index, count, component, signed))
                .collect::<Vec<_>>();
            let kind = if signed { naga::ScalarKind::Sint } else { naga::ScalarKind::Uint };
            components.push(scalar_literal(function, kind, 0));
            if count == 1 { components.push(scalar_literal(function, kind, 0)); }
            components.push(scalar_literal(function, kind, 1));
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::Rgba8Uint | TextureFormat::Rgba8Sint => {
            let components = (0..4)
                .map(|component| byte_component(function, words[0], component, format == TextureFormat::Rgba8Sint))
                .collect();
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::R32Float | TextureFormat::R32Uint | TextureFormat::R32Sint => {
            let kind = format_shape(format)?.0;
            let first = function.expressions.append(
                Expression::As {
                    expr: words[0],
                    kind,
                    convert: None,
                },
                Span::default(),
            );
            let zero = scalar_literal(function, kind, 0);
            let one = scalar_literal(function, kind, 1);
            function.expressions.append(
                Expression::Compose {
                    ty: vec4,
                    components: vec![first, zero, zero, one],
                },
                Span::default(),
            )
        }
        TextureFormat::Rgba16Float => {
            let low = unpack_half(function, words[0]);
            let high = unpack_half(function, words[1]);
            let mut components = Vec::with_capacity(4);
            for pair in [low, high] {
                for index in 0..2 {
                    components.push(function.expressions.append(
                        Expression::AccessIndex { base: pair, index },
                        Span::default(),
                    ));
                }
            }
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::Rg16Float => {
            let pair = unpack_half(function, words[0]);
            let components = vec![
                function.expressions.append(Expression::AccessIndex { base: pair, index: 0 }, Span::default()),
                function.expressions.append(Expression::AccessIndex { base: pair, index: 1 }, Span::default()),
                scalar_literal(function, naga::ScalarKind::Float, 0),
                scalar_literal(function, naga::ScalarKind::Float, 1),
            ];
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::R16Float => {
            let pair = unpack_half(function, words[0]);
            let lane = half_lane(function, pair, index);
            let zero = scalar_literal(function, naga::ScalarKind::Float, 0);
            let one = scalar_literal(function, naga::ScalarKind::Float, 1);
            function.expressions.append(Expression::Compose { ty: vec4, components: vec![lane, zero, zero, one] }, Span::default())
        }
        TextureFormat::R16Uint | TextureFormat::R16Sint | TextureFormat::Rg16Uint | TextureFormat::Rg16Sint | TextureFormat::Rgba16Uint | TextureFormat::Rgba16Sint => {
            let signed = matches!(format, TextureFormat::R16Sint | TextureFormat::Rg16Sint | TextureFormat::Rgba16Sint);
            let count = if matches!(format, TextureFormat::R16Uint | TextureFormat::R16Sint) { 1 } else if matches!(format, TextureFormat::Rg16Uint | TextureFormat::Rg16Sint) { 2 } else { 4 };
            let mut components = (0..count).map(|component| packed_half_component(function, words[component as usize / 2], index, count * 2, component % 2, signed)).collect::<Vec<_>>();
            let kind = if signed { naga::ScalarKind::Sint } else { naga::ScalarKind::Uint };
            while components.len() < 3 { components.push(scalar_literal(function, kind, 0)); }
            components.push(scalar_literal(function, kind, 1));
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::Rg32Float | TextureFormat::Rg32Uint | TextureFormat::Rg32Sint => {
            let kind = format_shape(format)?.0;
            let mut components = words[..2].iter().map(|word| function.expressions.append(Expression::As { expr: *word, kind, convert: None }, Span::default())).collect::<Vec<_>>();
            components.push(scalar_literal(function, kind, 0));
            components.push(scalar_literal(function, kind, 1));
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        TextureFormat::Rgba32Float | TextureFormat::Rgba32Uint | TextureFormat::Rgba32Sint => {
            let kind = format_shape(format)?.0;
            let components = words
                .iter()
                .map(|word| function.expressions.append(Expression::As { expr: *word, kind, convert: None }, Span::default()))
                .collect();
            function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default())
        }
        _ => return Err(GpuError::Unsupported("wgpu: raw texel decode format")),
    };
    if format == TextureFormat::Bgra8Unorm {
        let components = [2, 1, 0, 3]
            .map(|index| function.expressions.append(Expression::AccessIndex { base: value, index }, Span::default()))
            .to_vec();
        Ok(function.expressions.append(Expression::Compose { ty: vec4, components }, Span::default()))
    } else {
        Ok(value)
    }
}

fn unpack_half(function: &mut naga::Function, word: Handle<Expression>) -> Handle<Expression> {
    function.expressions.append(
        Expression::Math {
            fun: naga::MathFunction::Unpack2x16float,
            arg: word,
            arg1: None,
            arg2: None,
            arg3: None,
        },
        Span::default(),
    )
}

fn half_lane(function: &mut naga::Function, pair: Handle<Expression>, index: Handle<Expression>) -> Handle<Expression> {
    let index = function.expressions.append(Expression::As { expr: index, kind: naga::ScalarKind::Uint, convert: Some(4) }, Span::default());
    let two = function.expressions.append(Expression::Literal(naga::Literal::U32(2)), Span::default());
    let lane = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Modulo, left: index, right: two }, Span::default());
    function.expressions.append(Expression::Access { base: pair, index: lane }, Span::default())
}

fn packed_half_component(function: &mut naga::Function, word: Handle<Expression>, index: Handle<Expression>, bytes: u32, component: u32, signed: bool) -> Handle<Expression> {
    let index = function.expressions.append(Expression::As { expr: index, kind: naga::ScalarKind::Uint, convert: Some(4) }, Span::default());
    let bytes = function.expressions.append(Expression::Literal(naga::Literal::U32(bytes)), Span::default());
    let offset = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: index, right: bytes }, Span::default());
    let four = function.expressions.append(Expression::Literal(naga::Literal::U32(4)), Span::default());
    let offset = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Modulo, left: offset, right: four }, Span::default());
    let component = function.expressions.append(Expression::Literal(naga::Literal::U32(component * 2)), Span::default());
    let offset = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Add, left: offset, right: component }, Span::default());
    let eight = function.expressions.append(Expression::Literal(naga::Literal::U32(8)), Span::default());
    let shift = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: offset, right: eight }, Span::default());
    let shifted = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftRight, left: word, right: shift }, Span::default());
    let mask = function.expressions.append(Expression::Literal(naga::Literal::U32(0xffff)), Span::default());
    let half = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: shifted, right: mask }, Span::default());
    if !signed { return half; }
    let shift_16 = function.expressions.append(Expression::Literal(naga::Literal::U32(16)), Span::default());
    let left = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: half, right: shift_16 }, Span::default());
    let signed = function.expressions.append(Expression::As { expr: left, kind: naga::ScalarKind::Sint, convert: None }, Span::default());
    function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftRight, left: signed, right: shift_16 }, Span::default())
}

fn byte_component(
    function: &mut naga::Function,
    word: Handle<Expression>,
    component: u32,
    signed: bool,
) -> Handle<Expression> {
    let shift = function.expressions.append(
        Expression::Literal(naga::Literal::U32(component * 8)),
        Span::default(),
    );
    let shifted = function.expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::ShiftRight,
            left: word,
            right: shift,
        },
        Span::default(),
    );
    let mask = function.expressions.append(
        Expression::Literal(naga::Literal::U32(0xff)),
        Span::default(),
    );
    let byte = function.expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::And,
            left: shifted,
            right: mask,
        },
        Span::default(),
    );
    if signed {
        let shift_24 = function.expressions.append(
            Expression::Literal(naga::Literal::U32(24)),
            Span::default(),
        );
        let left = function.expressions.append(
            Expression::Binary {
                op: naga::BinaryOperator::ShiftLeft,
                left: byte,
                right: shift_24,
            },
            Span::default(),
        );
        let signed = function.expressions.append(Expression::As { expr: left, kind: naga::ScalarKind::Sint, convert: None }, Span::default());
        function.expressions.append(
            Expression::Binary {
                op: naga::BinaryOperator::ShiftRight,
                left: signed,
                right: shift_24,
            },
            Span::default(),
        )
    } else {
        byte
    }
}

fn packed_byte_component(
    function: &mut naga::Function,
    word: Handle<Expression>,
    index: Handle<Expression>,
    bytes: u32,
    component: u32,
    signed: bool,
) -> Handle<Expression> {
    let index = function.expressions.append(Expression::As { expr: index, kind: naga::ScalarKind::Uint, convert: Some(4) }, Span::default());
    let bytes_expr = function.expressions.append(Expression::Literal(naga::Literal::U32(bytes)), Span::default());
    let byte = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: index, right: bytes_expr }, Span::default());
    let four = function.expressions.append(Expression::Literal(naga::Literal::U32(4)), Span::default());
    let byte = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Modulo, left: byte, right: four }, Span::default());
    let component = function.expressions.append(Expression::Literal(naga::Literal::U32(component)), Span::default());
    let byte = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Add, left: byte, right: component }, Span::default());
    let eight = function.expressions.append(Expression::Literal(naga::Literal::U32(8)), Span::default());
    let shift = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: byte, right: eight }, Span::default());
    shifted_byte(function, word, shift, signed)
}

fn shifted_byte(function: &mut naga::Function, word: Handle<Expression>, shift: Handle<Expression>, signed: bool) -> Handle<Expression> {
    let shifted = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftRight, left: word, right: shift }, Span::default());
    let mask = function.expressions.append(Expression::Literal(naga::Literal::U32(0xff)), Span::default());
    let byte = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: shifted, right: mask }, Span::default());
    if !signed { return byte; }
    let shift_24 = function.expressions.append(Expression::Literal(naga::Literal::U32(24)), Span::default());
    let left = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: byte, right: shift_24 }, Span::default());
    let signed = function.expressions.append(Expression::As { expr: left, kind: naga::ScalarKind::Sint, convert: None }, Span::default());
    function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftRight, left: signed, right: shift_24 }, Span::default())
}

fn normalized_byte(function: &mut naga::Function, word: Handle<Expression>, index: Handle<Expression>, bytes: u32, component: u32, signed: bool) -> Handle<Expression> {
    let byte = packed_byte_component(function, word, index, bytes, component, signed);
    let float = function.expressions.append(Expression::As { expr: byte, kind: naga::ScalarKind::Float, convert: Some(4) }, Span::default());
    let max = function.expressions.append(Expression::Literal(naga::Literal::F32(if signed { 127.0 } else { 255.0 })), Span::default());
    let value = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Divide, left: float, right: max }, Span::default());
    if !signed { return value; }
    let neg_one = function.expressions.append(Expression::Literal(naga::Literal::F32(-1.0)), Span::default());
    function.expressions.append(Expression::Math { fun: naga::MathFunction::Max, arg: value, arg1: Some(neg_one), arg2: None, arg3: None }, Span::default())
}

fn scalar_literal(
    function: &mut naga::Function,
    kind: naga::ScalarKind,
    value: u32,
) -> Handle<Expression> {
    let literal = match kind {
        naga::ScalarKind::Float => naga::Literal::F32(value as f32),
        naga::ScalarKind::Sint => naga::Literal::I32(value as i32),
        naga::ScalarKind::Uint => naga::Literal::U32(value),
        _ => unreachable!("texel scalar kind"),
    };
    function.expressions.append(Expression::Literal(literal), Span::default())
}

pub(super) fn encode(
    function: &mut naga::Function,
    format: TextureFormat,
    index: Handle<Expression>,
    value: Handle<Expression>,
    current: &[Handle<Expression>],
) -> Result<Vec<Handle<Expression>>> {
    let packed = match format {
        TextureFormat::R8Unorm | TextureFormat::Rg8Unorm | TextureFormat::Rg8Snorm | TextureFormat::R8Uint | TextureFormat::Rg8Uint | TextureFormat::R8Sint | TextureFormat::Rg8Sint => {
            let bytes = if matches!(format, TextureFormat::R8Unorm | TextureFormat::R8Uint | TextureFormat::R8Sint) { 1 } else { 2 };
            vec![encode_partial_bytes(function, format, index, value, current[0], bytes)]
        }
        TextureFormat::Rgba8Unorm | TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Snorm => {
            let source = if format == TextureFormat::Bgra8Unorm {
                function.expressions.append(
                    Expression::Swizzle {
                        size: naga::VectorSize::Quad,
                        vector: value,
                        pattern: [
                            naga::SwizzleComponent::Z,
                            naga::SwizzleComponent::Y,
                            naga::SwizzleComponent::X,
                            naga::SwizzleComponent::W,
                        ],
                    },
                    Span::default(),
                )
            } else {
                value
            };
            vec![function.expressions.append(
                Expression::Math {
                    fun: if format == TextureFormat::Rgba8Snorm { naga::MathFunction::Pack4x8snorm } else { naga::MathFunction::Pack4x8unorm },
                    arg: source,
                    arg1: None,
                    arg2: None,
                    arg3: None,
                },
                Span::default(),
            )]
        }
        TextureFormat::Rgba8Uint | TextureFormat::Rgba8Sint => {
            let mut word = function.expressions.append(Expression::Literal(naga::Literal::U32(0)), Span::default());
            for component in 0..4 {
                let part = function.expressions.append(Expression::AccessIndex { base: value, index: component }, Span::default());
                let part = function.expressions.append(Expression::As { expr: part, kind: naga::ScalarKind::Uint, convert: None }, Span::default());
                let mask = function.expressions.append(Expression::Literal(naga::Literal::U32(0xff)), Span::default());
                let part = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: part, right: mask }, Span::default());
                let shift = function.expressions.append(Expression::Literal(naga::Literal::U32(component * 8)), Span::default());
                let part = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: part, right: shift }, Span::default());
                word = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::InclusiveOr, left: word, right: part }, Span::default());
            }
            vec![word]
        }
        TextureFormat::R32Float | TextureFormat::R32Uint | TextureFormat::R32Sint => {
            let first = function.expressions.append(Expression::AccessIndex { base: value, index: 0 }, Span::default());
            vec![function.expressions.append(Expression::As { expr: first, kind: naga::ScalarKind::Uint, convert: None }, Span::default())]
        }
        TextureFormat::Rgba16Float => {
            [
                [naga::SwizzleComponent::X, naga::SwizzleComponent::Y],
                [naga::SwizzleComponent::Z, naga::SwizzleComponent::W],
            ]
            .into_iter()
            .map(|components| {
                let pair = function.expressions.append(
                    Expression::Swizzle {
                        size: naga::VectorSize::Bi,
                        vector: value,
                        pattern: [
                            components[0],
                            components[1],
                            naga::SwizzleComponent::X,
                            naga::SwizzleComponent::X,
                        ],
                    },
                    Span::default(),
                );
                function.expressions.append(
                    Expression::Math {
                        fun: naga::MathFunction::Pack2x16float,
                        arg: pair,
                        arg1: None,
                        arg2: None,
                        arg3: None,
                    },
                    Span::default(),
                )
            })
            .collect()
        }
        TextureFormat::Rgba16Uint | TextureFormat::Rgba16Sint => (0..2).map(|pair| {
            let mut word = function.expressions.append(Expression::Literal(naga::Literal::U32(0)), Span::default());
            for lane in 0..2 {
                let component = function.expressions.append(Expression::AccessIndex { base: value, index: pair * 2 + lane }, Span::default());
                let component = function.expressions.append(Expression::As { expr: component, kind: naga::ScalarKind::Uint, convert: None }, Span::default());
                let mask = function.expressions.append(Expression::Literal(naga::Literal::U32(0xffff)), Span::default());
                let component = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: component, right: mask }, Span::default());
                let shift = function.expressions.append(Expression::Literal(naga::Literal::U32(lane * 16)), Span::default());
                let component = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: component, right: shift }, Span::default());
                word = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::InclusiveOr, left: word, right: component }, Span::default());
            }
            word
        }).collect(),
        TextureFormat::Rg16Float => {
            let pair = function.expressions.append(Expression::Swizzle {
                size: naga::VectorSize::Bi, vector: value,
                pattern: [naga::SwizzleComponent::X, naga::SwizzleComponent::Y, naga::SwizzleComponent::X, naga::SwizzleComponent::X],
            }, Span::default());
            vec![function.expressions.append(Expression::Math {
                fun: naga::MathFunction::Pack2x16float, arg: pair, arg1: None, arg2: None, arg3: None,
            }, Span::default())]
        }
        TextureFormat::Rg32Float | TextureFormat::Rg32Uint | TextureFormat::Rg32Sint
        | TextureFormat::Rgba32Float | TextureFormat::Rgba32Uint | TextureFormat::Rgba32Sint => (0..if matches!(format, TextureFormat::Rg32Float | TextureFormat::Rg32Uint | TextureFormat::Rg32Sint) { 2 } else { 4 })
            .map(|index| {
                let component = function.expressions.append(Expression::AccessIndex { base: value, index }, Span::default());
                function.expressions.append(Expression::As { expr: component, kind: naga::ScalarKind::Uint, convert: None }, Span::default())
            })
            .collect(),
        _ => return Err(GpuError::Unsupported("wgpu: raw texel encode format")),
    };
    Ok(packed)
}

pub(super) fn encode_partial_bytes(
    function: &mut naga::Function,
    format: TextureFormat,
    index: Handle<Expression>,
    value: Handle<Expression>,
    current: Handle<Expression>,
    bytes: u32,
) -> Handle<Expression> {
    let mut packed = if matches!(format, TextureFormat::R8Unorm | TextureFormat::Rg8Unorm | TextureFormat::Rg8Snorm) {
        function.expressions.append(
            Expression::Math {
                fun: if format == TextureFormat::Rg8Snorm { naga::MathFunction::Pack4x8snorm } else { naga::MathFunction::Pack4x8unorm },
                arg: value,
                arg1: None,
                arg2: None,
                arg3: None,
            },
            Span::default(),
        )
    } else {
        function.expressions.append(Expression::Literal(naga::Literal::U32(0)), Span::default())
    };
    if !matches!(format, TextureFormat::R8Unorm | TextureFormat::Rg8Unorm | TextureFormat::Rg8Snorm) {
        for component in 0..bytes {
            let part = function.expressions.append(Expression::AccessIndex { base: value, index: component }, Span::default());
            let part = function.expressions.append(Expression::As { expr: part, kind: naga::ScalarKind::Uint, convert: None }, Span::default());
            let byte_mask = function.expressions.append(Expression::Literal(naga::Literal::U32(0xff)), Span::default());
            let part = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: part, right: byte_mask }, Span::default());
            let shift = function.expressions.append(Expression::Literal(naga::Literal::U32(component * 8)), Span::default());
            let part = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: part, right: shift }, Span::default());
            packed = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::InclusiveOr, left: packed, right: part }, Span::default());
        }
    }
    let index = function.expressions.append(Expression::As { expr: index, kind: naga::ScalarKind::Uint, convert: Some(4) }, Span::default());
    let bytes_expr = function.expressions.append(Expression::Literal(naga::Literal::U32(bytes)), Span::default());
    let byte_offset = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: index, right: bytes_expr }, Span::default());
    let four = function.expressions.append(Expression::Literal(naga::Literal::U32(4)), Span::default());
    let byte_offset = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Modulo, left: byte_offset, right: four }, Span::default());
    let eight = function.expressions.append(Expression::Literal(naga::Literal::U32(8)), Span::default());
    let shift = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: byte_offset, right: eight }, Span::default());
    let low_mask = function.expressions.append(Expression::Literal(naga::Literal::U32((1u32 << (bytes * 8)) - 1)), Span::default());
    let mask = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: low_mask, right: shift }, Span::default());
    let inverse = function.expressions.append(Expression::Unary { op: naga::UnaryOperator::BitwiseNot, expr: mask }, Span::default());
    let preserved = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: current, right: inverse }, Span::default());
    let packed = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::And, left: packed, right: low_mask }, Span::default());
    let packed = function.expressions.append(Expression::Binary { op: naga::BinaryOperator::ShiftLeft, left: packed, right: shift }, Span::default());
    function.expressions.append(Expression::Binary { op: naga::BinaryOperator::InclusiveOr, left: preserved, right: packed }, Span::default())
}
