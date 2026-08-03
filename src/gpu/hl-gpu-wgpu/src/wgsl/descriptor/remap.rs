use std::mem;

use naga::{Arena, Block, Expression, Handle, Range, Statement};

pub(in crate::wgsl) fn dedup_emits(block: &mut Block, expressions: &Arena<Expression>) {
    dedup_block(block, expressions, &std::collections::BTreeSet::new());
}

fn dedup_block(
    block: &mut Block,
    expressions: &Arena<Expression>,
    inherited: &std::collections::BTreeSet<usize>,
) {
    let mut emitted = inherited.clone();
    let mut rebuilt = Block::with_capacity(block.len());
    for (mut statement, span) in mem::take(block).span_into_iter() {
        if let Statement::Emit(range) = &mut statement {
            let handles = range
                .clone()
                .filter(|handle| {
                    !expressions[*handle].needs_pre_emit() && emitted.insert(handle.index())
                })
                .collect::<Vec<_>>();
            let mut start = None;
            let mut previous = None;
            for handle in handles {
                if previous
                    .is_some_and(|prior: Handle<Expression>| prior.index() + 1 != handle.index())
                {
                    rebuilt.push(
                        Statement::Emit(Range::new_from_bounds(start.unwrap(), previous.unwrap())),
                        span,
                    );
                    start = None;
                }
                start.get_or_insert(handle);
                previous = Some(handle);
            }
            if let (Some(first), Some(last)) = (start, previous) {
                rebuilt.push(Statement::Emit(Range::new_from_bounds(first, last)), span);
            }
        } else {
            match &mut statement {
                Statement::Block(nested) => dedup_block(nested, expressions, &emitted),
                Statement::If { accept, reject, .. } => {
                    dedup_block(accept, expressions, &emitted);
                    dedup_block(reject, expressions, &emitted);
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        dedup_block(&mut case.body, expressions, &emitted);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    dedup_block(body, expressions, &emitted);
                    dedup_block(continuing, expressions, &emitted);
                }
                _ => {}
            }
            rebuilt.push(statement, span);
        }
    }
    *block = rebuilt;
}

pub(in crate::wgsl) fn expression(map: &[Handle<Expression>], expression: &mut Expression) {
    let remap = |handle: &mut Handle<Expression>| *handle = map[handle.index()];
    match expression {
        Expression::Compose { components, .. } => components.iter_mut().for_each(remap),
        Expression::Access { base, index } => {
            remap(base);
            remap(index);
        }
        Expression::AccessIndex { base, .. }
        | Expression::Splat { value: base, .. }
        | Expression::Swizzle { vector: base, .. }
        | Expression::Load { pointer: base }
        | Expression::Unary { expr: base, .. }
        | Expression::Derivative { expr: base, .. }
        | Expression::Relational { argument: base, .. }
        | Expression::As { expr: base, .. }
        | Expression::ArrayLength(base) => remap(base),
        Expression::Binary { left, right, .. } => {
            remap(left);
            remap(right);
        }
        Expression::Select {
            condition,
            accept,
            reject,
        } => {
            remap(condition);
            remap(accept);
            remap(reject);
        }
        Expression::Math {
            arg,
            arg1,
            arg2,
            arg3,
            ..
        } => {
            remap(arg);
            arg1.iter_mut().for_each(remap);
            arg2.iter_mut().for_each(remap);
            arg3.iter_mut().for_each(remap);
        }
        Expression::ImageSample {
            image,
            sampler,
            coordinate,
            array_index,
            offset,
            level,
            depth_ref,
            ..
        } => {
            remap(image);
            remap(sampler);
            remap(coordinate);
            array_index.iter_mut().for_each(remap);
            offset.iter_mut().for_each(remap);
            match level {
                naga::SampleLevel::Exact(value) | naga::SampleLevel::Bias(value) => remap(value),
                naga::SampleLevel::Gradient { x, y } => {
                    remap(x);
                    remap(y);
                }
                _ => {}
            }
            depth_ref.iter_mut().for_each(remap);
        }
        Expression::ImageLoad {
            image,
            coordinate,
            array_index,
            sample,
            level,
        } => {
            remap(image);
            remap(coordinate);
            array_index.iter_mut().for_each(remap);
            sample.iter_mut().for_each(remap);
            level.iter_mut().for_each(remap);
        }
        Expression::ImageQuery { image, query } => {
            remap(image);
            if let naga::ImageQuery::Size { level: Some(level) } = query {
                remap(level);
            }
        }
        Expression::RayQueryGetIntersection { query, .. } => remap(query),
        Expression::Literal(_)
        | Expression::Constant(_)
        | Expression::Override(_)
        | Expression::ZeroValue(_)
        | Expression::FunctionArgument(_)
        | Expression::GlobalVariable(_)
        | Expression::LocalVariable(_)
        | Expression::CallResult(_)
        | Expression::AtomicResult { .. }
        | Expression::WorkGroupUniformLoadResult { .. }
        | Expression::RayQueryProceedResult
        | Expression::SubgroupBallotResult
        | Expression::SubgroupOperationResult { .. } => {}
    }
}

pub(super) fn block(
    map: &[Handle<Expression>],
    spans: &[(Handle<Expression>, Handle<Expression>)],
    block: &mut Block,
) {
    for item in block.iter_mut() {
        statement(map, spans, item);
    }
}

pub(in crate::wgsl) fn statement(
    map: &[Handle<Expression>],
    spans: &[(Handle<Expression>, Handle<Expression>)],
    statement: &mut Statement,
) {
    let remap = |handle: &mut Handle<Expression>| *handle = map[handle.index()];
    match statement {
        Statement::Emit(range) => {
            if let Some((first, last)) = range.first_and_last() {
                *range = Range::new_from_bounds(spans[first.index()].0, spans[last.index()].1);
            }
        }
        Statement::Block(nested) => block(map, spans, nested),
        Statement::If {
            condition,
            accept,
            reject,
        } => {
            remap(condition);
            block(map, spans, accept);
            block(map, spans, reject);
        }
        Statement::Switch { selector, cases } => {
            remap(selector);
            cases
                .iter_mut()
                .for_each(|case| block(map, spans, &mut case.body));
        }
        Statement::Loop {
            body,
            continuing,
            break_if,
        } => {
            block(map, spans, body);
            block(map, spans, continuing);
            break_if.iter_mut().for_each(remap);
        }
        Statement::Return { value } => value.iter_mut().for_each(remap),
        Statement::Store { pointer, value } => {
            remap(pointer);
            remap(value);
        }
        Statement::Call {
            arguments, result, ..
        } => {
            arguments.iter_mut().for_each(remap);
            result.iter_mut().for_each(remap);
        }
        Statement::ImageStore {
            image,
            coordinate,
            array_index,
            value,
        } => {
            remap(image);
            remap(coordinate);
            array_index.iter_mut().for_each(remap);
            remap(value);
        }
        Statement::Atomic {
            pointer,
            value,
            result,
            fun,
        } => {
            remap(pointer);
            remap(value);
            result.iter_mut().for_each(remap);
            if let naga::AtomicFunction::Exchange {
                compare: Some(compare),
            } = fun
            {
                remap(compare);
            }
        }
        Statement::ImageAtomic {
            image,
            coordinate,
            array_index,
            value,
            result,
            fun,
        } => {
            remap(image);
            remap(coordinate);
            array_index.iter_mut().for_each(remap);
            remap(value);
            result.iter_mut().for_each(remap);
            if let naga::AtomicFunction::Exchange {
                compare: Some(compare),
            } = fun
            {
                remap(compare);
            }
        }
        Statement::WorkGroupUniformLoad { pointer, result } => {
            remap(pointer);
            remap(result);
        }
        Statement::SubgroupBallot { result, predicate } => {
            remap(result);
            predicate.iter_mut().for_each(remap);
        }
        Statement::SubgroupCollectiveOperation {
            argument, result, ..
        } => {
            remap(argument);
            remap(result);
        }
        Statement::SubgroupGather {
            mode,
            argument,
            result,
        } => {
            match mode {
                naga::GatherMode::BroadcastFirst => {}
                naga::GatherMode::Broadcast(index)
                | naga::GatherMode::Shuffle(index)
                | naga::GatherMode::ShuffleDown(index)
                | naga::GatherMode::ShuffleUp(index)
                | naga::GatherMode::ShuffleXor(index) => remap(index),
            }
            remap(argument);
            remap(result);
        }
        Statement::RayQuery { query, fun } => {
            remap(query);
            match fun {
                naga::RayQueryFunction::Initialize {
                    acceleration_structure,
                    descriptor,
                } => {
                    remap(acceleration_structure);
                    remap(descriptor);
                }
                naga::RayQueryFunction::Proceed { result } => remap(result),
                naga::RayQueryFunction::Terminate => {}
            }
        }
        Statement::Break | Statement::Continue | Statement::Kill | Statement::Barrier(_) => {}
    }
}
