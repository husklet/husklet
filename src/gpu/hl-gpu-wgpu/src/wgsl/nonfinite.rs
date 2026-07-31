//! Lower `isInf` / `isNan` to integer tests on the IEEE-754 binary32 bit pattern, in naga's OWN IR.
//!
//! naga's `wgsl-out` emits no relational function except `all`/`any`, because WGSL has no `isNan`/`isInf`
//! at all — gpuweb removed them (gpuweb#2311) on the grounds that backends assuming fast math made them
//! unreliable. Any module still carrying `RelationalFunction::IsInf`/`IsNan` is therefore refused at WGSL
//! emission, whatever front end produced it.
//!
//! The GLSL route avoids that by rewriting the source before naga parses
//! (`glsl_es::Source::rewrite_nonfinite_predicates`). That rewrite is TEXTUAL, so it cannot reach a
//! **SPIR-V** payload: `OpIsInf`/`OpIsNan` survive `spv-in` as these very expressions and hit the same
//! wall. A precompiled-shader guest using either predicate was refused outright. This pass closes that,
//! and it operates on the representation both front ends share, so it is the general form of the fix.
//!
//! WHY THIS IS A REBUILD AND NOT AN EDIT. The replacement is a small expression TREE — bitcast, mask,
//! compare — and naga requires every expression to reference only EARLIER handles in its arena (the same
//! `ForwardDependency` rule that forces `reorder_functions_topologically` next door). Appending the new
//! sub-expressions and pointing the old slot at them would point BACKWARDS from an earlier index to later
//! ones, which the validator rejects. So each function's expression arena is rebuilt in order, inserting
//! the replacement immediately before the expression it replaces, and every handle that referred into the
//! old arena is remapped. The insertion is monotonic, which is what keeps the remap a simple lookup and
//! keeps `Emit` ranges expressible: a range's bounds are remapped through the same table.
//!
//! The remaps below are EXHAUSTIVE matches on purpose. A missed variant would silently drop or misdirect a
//! handle and produce a shader that compiles and computes the wrong thing — the exact failure mode this
//! crate keeps finding — so the compiler is made to enforce completeness instead of review catching it.

use naga::{Block, Expression, Handle, Literal, Range, ScalarKind, Statement};

/// The masked exponent field of an IEEE-754 binary32: all-ones exponent with the sign removed.
const EXPONENT_ALL_ONES: u32 = 0x7f80_0000;
/// Everything but the sign bit.
const SIGN_MASK: u32 = 0x7fff_ffff;

pub(super) struct NonFinite;

impl NonFinite {
    /// Rewrite every `IsInf`/`IsNan` in the module. A module using neither is left untouched.
    pub(super) fn lower(module: &mut naga::Module) {
        for (_, function) in module.functions.iter_mut() {
            Self::lower_function(function);
        }
        for entry_point in module.entry_points.iter_mut() {
            Self::lower_function(&mut entry_point.function);
        }
    }

    fn is_target(expression: &Expression) -> bool {
        matches!(
            expression,
            Expression::Relational {
                fun: naga::RelationalFunction::IsInf | naga::RelationalFunction::IsNan,
                ..
            }
        )
    }

    fn lower_function(function: &mut naga::Function) {
        if !function
            .expressions
            .iter()
            .any(|(_, expression)| Self::is_target(expression))
        {
            return; // byte-faithful for the overwhelmingly common case
        }

        let old = std::mem::take(&mut function.expressions);
        let mut new: naga::Arena<Expression> = naga::Arena::new();
        // The two constants are hoisted to the FRONT of the arena and shared by every rewrite in this
        // function. They must not land inside an `Emit` range: naga classifies a literal as const, so it is
        // always in scope, and emitting one is `ExpressionAlreadyInScope`. Placing them ahead of every
        // remapped expression puts them outside every range by construction, since no old expression maps
        // into this prefix.
        let mask = new.append(
            Expression::Literal(Literal::U32(SIGN_MASK)),
            naga::Span::default(),
        );
        let exponent = new.append(
            Expression::Literal(Literal::U32(EXPONENT_ALL_ONES)),
            naga::Span::default(),
        );
        // `value[i]` is what old handle `i` now evaluates to; `start[i]` is the FIRST handle emitted for
        // it. They differ only for a replaced predicate, whose replacement occupies several handles — and
        // an `Emit` range that began at such an expression must begin at the first of them, or the
        // intermediate values are used before they are in scope.
        let mut value: Vec<Handle<Expression>> = Vec::with_capacity(old.len());
        let mut start: Vec<Handle<Expression>> = Vec::with_capacity(old.len());

        for (handle, expression) in old.iter() {
            let span = old.get_span(handle);
            match expression {
                Expression::Relational { fun, argument }
                    if matches!(
                        fun,
                        naga::RelationalFunction::IsInf | naga::RelationalFunction::IsNan
                    ) =>
                {
                    let argument = value[argument.index()];
                    // `bitcast<u32>(x) & 0x7fffffff`, then compare against the all-ones exponent: EQUAL is
                    // an infinity (zero mantissa), GREATER is a NaN (nonzero mantissa). Masking the sign is
                    // what makes both infinities answer true.
                    let bits = new.append(
                        Expression::As {
                            expr: argument,
                            kind: ScalarKind::Uint,
                            convert: None, // no `convert` = reinterpret the bits, not a numeric cast
                        },
                        span,
                    );
                    let magnitude = new.append(
                        Expression::Binary {
                            op: naga::BinaryOperator::And,
                            left: bits,
                            right: mask,
                        },
                        span,
                    );
                    let op = match fun {
                        naga::RelationalFunction::IsNan => naga::BinaryOperator::Greater,
                        _ => naga::BinaryOperator::Equal,
                    };
                    let result = new.append(
                        Expression::Binary {
                            op,
                            left: magnitude,
                            right: exponent,
                        },
                        span,
                    );
                    value.push(result);
                    start.push(bits);
                }
                other => {
                    let handle = new.append(Self::remap_expression(other, &value), span);
                    value.push(handle);
                    start.push(handle);
                }
            }
        }

        function.expressions = new;
        Self::remap_block(&mut function.body, &value, &start);
        for (_, local) in function.local_variables.iter_mut() {
            // A local's initializer indexes this same arena (naga validates it against
            // `ExpressionKindTracker::from_arena(&fun.expressions)`).
            if let Some(init) = local.init.as_mut() {
                *init = value[init.index()];
            }
        }
        function.named_expressions = function
            .named_expressions
            .iter()
            .map(|(handle, name)| (value[handle.index()], name.clone()))
            .collect();
    }

    /// Rebuild one expression with every handle it carries remapped. EXHAUSTIVE — see the module header.
    fn remap_expression(expression: &Expression, value: &[Handle<Expression>]) -> Expression {
        let at = |handle: &Handle<Expression>| value[handle.index()];
        let maybe = |handle: &Option<Handle<Expression>>| handle.as_ref().map(at);
        match expression {
            // No expression handles to carry.
            Expression::Literal(literal) => Expression::Literal(*literal),
            Expression::Constant(handle) => Expression::Constant(*handle),
            Expression::Override(handle) => Expression::Override(*handle),
            Expression::ZeroValue(ty) => Expression::ZeroValue(*ty),
            Expression::FunctionArgument(index) => Expression::FunctionArgument(*index),
            Expression::GlobalVariable(handle) => Expression::GlobalVariable(*handle),
            Expression::LocalVariable(handle) => Expression::LocalVariable(*handle),
            Expression::CallResult(handle) => Expression::CallResult(*handle),
            Expression::AtomicResult { ty, comparison } => Expression::AtomicResult {
                ty: *ty,
                comparison: *comparison,
            },
            Expression::WorkGroupUniformLoadResult { ty } => {
                Expression::WorkGroupUniformLoadResult { ty: *ty }
            }
            Expression::RayQueryProceedResult => Expression::RayQueryProceedResult,
            Expression::SubgroupBallotResult => Expression::SubgroupBallotResult,
            Expression::SubgroupOperationResult { ty } => {
                Expression::SubgroupOperationResult { ty: *ty }
            }

            Expression::Compose { ty, components } => Expression::Compose {
                ty: *ty,
                components: components.iter().map(at).collect(),
            },
            Expression::Access { base, index } => Expression::Access {
                base: at(base),
                index: at(index),
            },
            Expression::AccessIndex { base, index } => Expression::AccessIndex {
                base: at(base),
                index: *index,
            },
            Expression::Splat { size, value: inner } => Expression::Splat {
                size: *size,
                value: at(inner),
            },
            Expression::Swizzle {
                size,
                vector,
                pattern,
            } => Expression::Swizzle {
                size: *size,
                vector: at(vector),
                pattern: *pattern,
            },
            Expression::Load { pointer } => Expression::Load {
                pointer: at(pointer),
            },
            Expression::ImageSample {
                image,
                sampler,
                gather,
                coordinate,
                array_index,
                offset,
                level,
                depth_ref,
            } => Expression::ImageSample {
                image: at(image),
                sampler: at(sampler),
                gather: *gather,
                coordinate: at(coordinate),
                array_index: maybe(array_index),
                offset: maybe(offset),
                level: Self::remap_sample_level(level, value),
                depth_ref: maybe(depth_ref),
            },
            Expression::ImageLoad {
                image,
                coordinate,
                array_index,
                sample,
                level,
            } => Expression::ImageLoad {
                image: at(image),
                coordinate: at(coordinate),
                array_index: maybe(array_index),
                sample: maybe(sample),
                level: maybe(level),
            },
            Expression::ImageQuery { image, query } => Expression::ImageQuery {
                image: at(image),
                query: match query {
                    naga::ImageQuery::Size { level } => naga::ImageQuery::Size {
                        level: maybe(level),
                    },
                    other => *other,
                },
            },
            Expression::Unary { op, expr } => Expression::Unary {
                op: *op,
                expr: at(expr),
            },
            Expression::Binary { op, left, right } => Expression::Binary {
                op: *op,
                left: at(left),
                right: at(right),
            },
            Expression::Select {
                condition,
                accept,
                reject,
            } => Expression::Select {
                condition: at(condition),
                accept: at(accept),
                reject: at(reject),
            },
            Expression::Derivative { axis, ctrl, expr } => Expression::Derivative {
                axis: *axis,
                ctrl: *ctrl,
                expr: at(expr),
            },
            // A relational this pass does not rewrite (`All`/`Any`) still needs its argument remapped.
            Expression::Relational { fun, argument } => Expression::Relational {
                fun: *fun,
                argument: at(argument),
            },
            Expression::Math {
                fun,
                arg,
                arg1,
                arg2,
                arg3,
            } => Expression::Math {
                fun: *fun,
                arg: at(arg),
                arg1: maybe(arg1),
                arg2: maybe(arg2),
                arg3: maybe(arg3),
            },
            Expression::As {
                expr,
                kind,
                convert,
            } => Expression::As {
                expr: at(expr),
                kind: *kind,
                convert: *convert,
            },
            Expression::ArrayLength(handle) => Expression::ArrayLength(at(handle)),
            Expression::RayQueryGetIntersection { query, committed } => {
                Expression::RayQueryGetIntersection {
                    query: at(query),
                    committed: *committed,
                }
            }
        }
    }

    fn remap_sample_level(
        level: &naga::SampleLevel,
        value: &[Handle<Expression>],
    ) -> naga::SampleLevel {
        let at = |handle: &Handle<Expression>| value[handle.index()];
        match level {
            naga::SampleLevel::Auto => naga::SampleLevel::Auto,
            naga::SampleLevel::Zero => naga::SampleLevel::Zero,
            naga::SampleLevel::Exact(handle) => naga::SampleLevel::Exact(at(handle)),
            naga::SampleLevel::Bias(handle) => naga::SampleLevel::Bias(at(handle)),
            naga::SampleLevel::Gradient { x, y } => naga::SampleLevel::Gradient {
                x: at(x),
                y: at(y),
            },
        }
    }

    fn remap_atomic(function: &naga::AtomicFunction, value: &[Handle<Expression>]) -> naga::AtomicFunction {
        match function {
            naga::AtomicFunction::Exchange { compare } => naga::AtomicFunction::Exchange {
                compare: compare.as_ref().map(|handle| value[handle.index()]),
            },
            other => *other,
        }
    }

    /// Rebuild one block with every handle remapped. EXHAUSTIVE — see the module header.
    fn remap_block(block: &mut Block, value: &[Handle<Expression>], start: &[Handle<Expression>]) {
        let at = |handle: &Handle<Expression>| value[handle.index()];
        let maybe = |handle: &Option<Handle<Expression>>| handle.as_ref().map(at);
        for statement in block.iter_mut() {
            match statement {
                // The range must OPEN at the first handle emitted for its old first expression, so a
                // replaced predicate's intermediates are in scope, and CLOSE at the value of its old last.
                Statement::Emit(range) => {
                    if let Some((first, last)) = range.first_and_last() {
                        *range = Range::new_from_bounds(
                            start[first.index()],
                            value[last.index()],
                        );
                    }
                }
                Statement::Block(inner) => Self::remap_block(inner, value, start),
                Statement::If {
                    condition,
                    accept,
                    reject,
                } => {
                    *condition = at(condition);
                    Self::remap_block(accept, value, start);
                    Self::remap_block(reject, value, start);
                }
                Statement::Switch { selector, cases } => {
                    *selector = at(selector);
                    for case in cases.iter_mut() {
                        Self::remap_block(&mut case.body, value, start);
                    }
                }
                Statement::Loop {
                    body,
                    continuing,
                    break_if,
                } => {
                    Self::remap_block(body, value, start);
                    Self::remap_block(continuing, value, start);
                    *break_if = maybe(break_if);
                }
                Statement::Return { value: returned } => *returned = maybe(returned),
                Statement::Store { pointer, value: stored } => {
                    *pointer = at(pointer);
                    *stored = at(stored);
                }
                Statement::ImageStore {
                    image,
                    coordinate,
                    array_index,
                    value: stored,
                } => {
                    *image = at(image);
                    *coordinate = at(coordinate);
                    *array_index = maybe(array_index);
                    *stored = at(stored);
                }
                Statement::Atomic {
                    pointer,
                    fun,
                    value: operand,
                    result,
                } => {
                    *pointer = at(pointer);
                    *fun = Self::remap_atomic(fun, value);
                    *operand = at(operand);
                    *result = maybe(result);
                }
                Statement::ImageAtomic {
                    image,
                    coordinate,
                    array_index,
                    fun,
                    value: operand,
                } => {
                    *image = at(image);
                    *coordinate = at(coordinate);
                    *array_index = maybe(array_index);
                    *fun = Self::remap_atomic(fun, value);
                    *operand = at(operand);
                }
                Statement::WorkGroupUniformLoad { pointer, result } => {
                    *pointer = at(pointer);
                    *result = at(result);
                }
                Statement::Call {
                    function: _,
                    arguments,
                    result,
                } => {
                    for argument in arguments.iter_mut() {
                        *argument = at(argument);
                    }
                    *result = maybe(result);
                }
                Statement::RayQuery { query, fun } => {
                    *query = at(query);
                    match fun {
                        naga::RayQueryFunction::Initialize {
                            acceleration_structure,
                            descriptor,
                        } => {
                            *acceleration_structure = at(acceleration_structure);
                            *descriptor = at(descriptor);
                        }
                        naga::RayQueryFunction::Proceed { result } => *result = at(result),
                        naga::RayQueryFunction::Terminate => {}
                    }
                }
                Statement::SubgroupBallot { result, predicate } => {
                    *result = at(result);
                    *predicate = maybe(predicate);
                }
                Statement::SubgroupGather {
                    mode,
                    argument,
                    result,
                } => {
                    *mode = match mode {
                        naga::GatherMode::BroadcastFirst => naga::GatherMode::BroadcastFirst,
                        naga::GatherMode::Broadcast(handle) => {
                            naga::GatherMode::Broadcast(at(handle))
                        }
                        naga::GatherMode::Shuffle(handle) => naga::GatherMode::Shuffle(at(handle)),
                        naga::GatherMode::ShuffleDown(handle) => {
                            naga::GatherMode::ShuffleDown(at(handle))
                        }
                        naga::GatherMode::ShuffleUp(handle) => {
                            naga::GatherMode::ShuffleUp(at(handle))
                        }
                        naga::GatherMode::ShuffleXor(handle) => {
                            naga::GatherMode::ShuffleXor(at(handle))
                        }
                    };
                    *argument = at(argument);
                    *result = at(result);
                }
                Statement::SubgroupCollectiveOperation {
                    op: _,
                    collective_op: _,
                    argument,
                    result,
                } => {
                    *argument = at(argument);
                    *result = at(result);
                }
                // No expression handles.
                Statement::Break
                | Statement::Continue
                | Statement::Kill
                | Statement::Barrier(_) => {}
            }
        }
    }
}
