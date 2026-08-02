use naga::{Expression, Handle, Span};

pub(super) fn raw_words(
    function: &mut naga::Function,
    global: Handle<naga::GlobalVariable>,
    index: Handle<Expression>,
    bytes: u32,
    count: u32,
    prefix_words: u32,
) -> Vec<Handle<Expression>> {
    (0..count)
        .map(|word| {
            let pointer = raw_pointer(function, global, index, bytes, word, prefix_words);
            function
                .expressions
                .append(Expression::Load { pointer }, Span::default())
        })
        .collect()
}

pub(super) fn raw_pointer(
    function: &mut naga::Function,
    global: Handle<naga::GlobalVariable>,
    index: Handle<Expression>,
    bytes: u32,
    word: u32,
    prefix_words: u32,
) -> Handle<Expression> {
    let index = function.expressions.append(
        Expression::As {
            expr: index,
            kind: naga::ScalarKind::Uint,
            convert: Some(4),
        },
        Span::default(),
    );
    let source = function
        .expressions
        .append(Expression::GlobalVariable(global), Span::default());
    let field = function.expressions.append(
        Expression::AccessIndex {
            base: source,
            index: 0,
        },
        Span::default(),
    );
    let bytes = function
        .expressions
        .append(Expression::Literal(naga::Literal::U32(bytes)), Span::default());
    let base = function.expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::Multiply,
            left: index,
            right: bytes,
        },
        Span::default(),
    );
    let four = function
        .expressions
        .append(Expression::Literal(naga::Literal::U32(4)), Span::default());
    let base = function.expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::Divide,
            left: base,
            right: four,
        },
        Span::default(),
    );
    let offset = function.expressions.append(
        Expression::Literal(naga::Literal::U32(word + prefix_words)),
        Span::default(),
    );
    let at = function.expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::Add,
            left: base,
            right: offset,
        },
        Span::default(),
    );
    function
        .expressions
        .append(Expression::Access { base: field, index: at }, Span::default())
}
