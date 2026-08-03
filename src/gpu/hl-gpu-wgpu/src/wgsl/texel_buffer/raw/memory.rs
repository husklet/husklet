use naga::{Arena, Expression, Handle, Span};

pub(super) fn raw_words(
    function: &mut naga::Function,
    global: Handle<naga::GlobalVariable>,
    index: Handle<Expression>,
    bytes: u32,
    count: u32,
    prefix_words: u32,
    atomic_kind: naga::ScalarKind,
) -> Vec<Handle<Expression>> {
    (0..count)
        .map(|word| {
            let pointer = raw_pointer(function, global, index, bytes, word, prefix_words);
            let loaded = function
                .expressions
                .append(Expression::Load { pointer }, Span::default());
            if atomic_kind == naga::ScalarKind::Uint {
                loaded
            } else {
                function.expressions.append(
                    Expression::As {
                        expr: loaded,
                        kind: naga::ScalarKind::Uint,
                        convert: None,
                    },
                    Span::default(),
                )
            }
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
    raw_pointer_in(
        &mut function.expressions,
        global,
        index,
        bytes,
        word,
        prefix_words,
    )
}

pub(super) fn raw_pointer_in(
    expressions: &mut Arena<Expression>,
    global: Handle<naga::GlobalVariable>,
    index: Handle<Expression>,
    bytes: u32,
    word: u32,
    prefix_words: u32,
) -> Handle<Expression> {
    let index = expressions.append(
        Expression::As {
            expr: index,
            kind: naga::ScalarKind::Uint,
            convert: Some(4),
        },
        Span::default(),
    );
    let source = expressions.append(Expression::GlobalVariable(global), Span::default());
    let field = expressions.append(
        Expression::AccessIndex {
            base: source,
            index: 0,
        },
        Span::default(),
    );
    let bytes = expressions.append(
        Expression::Literal(naga::Literal::U32(bytes)),
        Span::default(),
    );
    let base = expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::Multiply,
            left: index,
            right: bytes,
        },
        Span::default(),
    );
    let four = expressions.append(Expression::Literal(naga::Literal::U32(4)), Span::default());
    let base = expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::Divide,
            left: base,
            right: four,
        },
        Span::default(),
    );
    let offset = expressions.append(
        Expression::Literal(naga::Literal::U32(word + prefix_words)),
        Span::default(),
    );
    let at = expressions.append(
        Expression::Binary {
            op: naga::BinaryOperator::Add,
            left: base,
            right: offset,
        },
        Span::default(),
    );
    expressions.append(
        Expression::Access {
            base: field,
            index: at,
        },
        Span::default(),
    )
}
