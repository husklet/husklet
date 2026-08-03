use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::Result;
use naga::{Expression, Handle, Span, Statement};

use super::format::encode_partial_bytes;
use super::{raw_pointer, scalar};

pub(super) fn build_partial_store(
    module: &mut naga::Module,
    function: &mut naga::Function,
    global: Handle<naga::GlobalVariable>,
    format: TextureFormat,
    index: Handle<Expression>,
    value: Handle<Expression>,
    bytes: u32,
    prefix_words: u32,
) -> Result<()> {
    let u32_ty = scalar(module, naga::ScalarKind::Uint);
    let current_local = function.local_variables.append(
        naga::LocalVariable {
            name: Some("current_word".into()),
            ty: u32_ty,
            init: None,
        },
        Span::default(),
    );
    let pointer = raw_pointer(function, global, index, bytes, 0, prefix_words);
    let current = function
        .expressions
        .append(Expression::Load { pointer }, Span::default());
    let local_pointer = function
        .expressions
        .append(Expression::LocalVariable(current_local), Span::default());
    function.body.push(
        Statement::Emit(naga::Range::new_from_bounds(index, local_pointer)),
        Span::default(),
    );
    function.body.push(
        Statement::Store {
            pointer: local_pointer,
            value: current,
        },
        Span::default(),
    );

    let loop_current = function.expressions.append(
        Expression::Load {
            pointer: local_pointer,
        },
        Span::default(),
    );
    let desired = encode_partial_bytes(function, format, index, value, loop_current, bytes);
    let result_ty = module.generate_predeclared_type(
        naga::PredeclaredType::AtomicCompareExchangeWeakResult(naga::Scalar::U32),
    );
    let result = function.expressions.append(
        Expression::AtomicResult {
            ty: result_ty,
            comparison: true,
        },
        Span::default(),
    );
    let old = function.expressions.append(
        Expression::AccessIndex {
            base: result,
            index: 0,
        },
        Span::default(),
    );
    let exchanged = function.expressions.append(
        Expression::AccessIndex {
            base: result,
            index: 1,
        },
        Span::default(),
    );
    let mut body = naga::Block::new();
    body.push(
        Statement::Emit(naga::Range::new_from_bounds(loop_current, desired)),
        Span::default(),
    );
    body.push(
        Statement::Atomic {
            pointer,
            fun: naga::AtomicFunction::Exchange {
                compare: Some(loop_current),
            },
            value: desired,
            result: Some(result),
        },
        Span::default(),
    );
    body.push(
        Statement::Emit(naga::Range::new_from_bounds(old, exchanged)),
        Span::default(),
    );
    body.push(
        Statement::Store {
            pointer: local_pointer,
            value: old,
        },
        Span::default(),
    );
    body.push(
        Statement::If {
            condition: exchanged,
            accept: naga::Block::from_vec(vec![Statement::Break]),
            reject: naga::Block::new(),
        },
        Span::default(),
    );
    function.body.push(
        Statement::Loop {
            body,
            continuing: naga::Block::new(),
            break_if: None,
        },
        Span::default(),
    );
    Ok(())
}
