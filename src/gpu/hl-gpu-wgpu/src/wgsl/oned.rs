//! Lower Vulkan 1D image operations onto one-row physical 2D images.

use std::mem;

use hl_gpu::Result;
use naga::{Arena, Expression, Handle, Literal, Span, Statement, Type, TypeInner, VectorSize};

use super::descriptor::remap;

pub(super) struct Images;

impl Images {
    pub(super) fn lower(module: &mut naga::Module) -> Result<()> {
        let image_types = module
            .types
            .iter()
            .filter_map(|(handle, ty)| {
                matches!(
                    ty.inner,
                    TypeInner::Image {
                        dim: naga::ImageDimension::D1,
                        ..
                    }
                )
                .then_some(handle)
            })
            .collect::<Vec<_>>();
        if image_types.is_empty() {
            return Ok(());
        }

        let globals = module
            .global_variables
            .iter()
            .filter_map(|(handle, variable)| image_types.contains(&variable.ty).then_some(handle))
            .collect::<Vec<_>>();
        let float2 = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Vector {
                    size: VectorSize::Bi,
                    scalar: naga::Scalar::F32,
                },
            },
            Span::default(),
        );
        let sint2 = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Vector {
                    size: VectorSize::Bi,
                    scalar: naga::Scalar::I32,
                },
            },
            Span::default(),
        );

        let mapped_types = image_types
            .iter()
            .map(|&image| {
                let mut ty = module.types[image].clone();
                let TypeInner::Image { dim, .. } = &mut ty.inner else {
                    unreachable!()
                };
                *dim = naga::ImageDimension::D2;
                (image, module.types.insert(ty, Span::default()))
            })
            .collect::<Vec<_>>();
        for (_, variable) in module.global_variables.iter_mut() {
            if let Some((_, mapped)) = mapped_types.iter().find(|(old, _)| *old == variable.ty) {
                variable.ty = *mapped;
            }
        }
        let function_roots = module
            .functions
            .iter_mut()
            .map(|(_, function)| {
                let roots = Roots::from_function(function, &image_types);
                Self::remap_function_types(function, &mapped_types);
                roots
            })
            .collect::<Vec<_>>();
        let entry_roots = module
            .entry_points
            .iter_mut()
            .map(|entry| {
                let roots = Roots::from_function(&entry.function, &image_types);
                Self::remap_function_types(&mut entry.function, &mapped_types);
                roots
            })
            .collect::<Vec<_>>();
        for ((_, function), roots) in module.functions.iter_mut().zip(function_roots) {
            FunctionLowering::new(&globals, roots, float2, sint2).lower(function)?;
        }
        for (entry, roots) in module.entry_points.iter_mut().zip(entry_roots) {
            FunctionLowering::new(&globals, roots, float2, sint2).lower(&mut entry.function)?;
        }
        Ok(())
    }

    fn remap_function_types(
        function: &mut naga::Function,
        mapped: &[(Handle<Type>, Handle<Type>)],
    ) {
        let remap = |ty: &mut Handle<Type>| {
            if let Some((_, replacement)) = mapped.iter().find(|(old, _)| old == ty) {
                *ty = *replacement;
            }
        };
        for argument in &mut function.arguments {
            remap(&mut argument.ty);
        }
        if let Some(result) = &mut function.result {
            remap(&mut result.ty);
        }
        for (_, local) in function.local_variables.iter_mut() {
            remap(&mut local.ty);
        }
    }
}

struct Roots {
    arguments: Vec<bool>,
    locals: Vec<Handle<naga::LocalVariable>>,
}

impl Roots {
    fn from_function(function: &naga::Function, image_types: &[Handle<Type>]) -> Self {
        Self {
            arguments: function
                .arguments
                .iter()
                .map(|argument| image_types.contains(&argument.ty))
                .collect(),
            locals: function
                .local_variables
                .iter()
                .filter_map(|(handle, local)| image_types.contains(&local.ty).then_some(handle))
                .collect(),
        }
    }
}

struct FunctionLowering<'a> {
    globals: &'a [Handle<naga::GlobalVariable>],
    arguments: Vec<bool>,
    locals: Vec<Handle<naga::LocalVariable>>,
    float2: Handle<Type>,
    sint2: Handle<Type>,
    map: Vec<Handle<Expression>>,
    spans: Vec<(Handle<Expression>, Handle<Expression>)>,
    oned: Vec<bool>,
}

impl<'a> FunctionLowering<'a> {
    fn new(
        globals: &'a [Handle<naga::GlobalVariable>],
        roots: Roots,
        float2: Handle<Type>,
        sint2: Handle<Type>,
    ) -> Self {
        Self {
            globals,
            arguments: roots.arguments,
            locals: roots.locals,
            float2,
            sint2,
            map: Vec::new(),
            spans: Vec::new(),
            oned: Vec::new(),
        }
    }

    fn lower(mut self, function: &mut naga::Function) -> Result<()> {
        let mut old = mem::take(&mut function.expressions);
        let mut expressions = Arena::new();
        for (old_handle, expression, span) in old.drain() {
            let first = expressions.len();
            let is_oned = self.is_oned(&expression);
            let mapped = self.expression(expression, span, &mut expressions);
            self.map.push(mapped);
            self.oned.push(is_oned);
            let last = expressions.len();
            self.spans.push(if last > first {
                (
                    expressions.iter().nth(first).unwrap().0,
                    expressions.iter().nth(last - 1).unwrap().0,
                )
            } else {
                (mapped, mapped)
            });
            debug_assert_eq!(old_handle.index() + 1, self.map.len());
        }
        self.block(&mut function.body, &mut expressions);
        remap::dedup_emits(&mut function.body, &expressions);
        for (_, local) in function.local_variables.iter_mut() {
            if let Some(init) = &mut local.init {
                *init = self.map[init.index()];
            }
        }
        function.named_expressions = mem::take(&mut function.named_expressions)
            .into_iter()
            .map(|(handle, name)| (self.map[handle.index()], name))
            .collect();
        function.expressions = expressions;
        Ok(())
    }

    fn is_oned(&self, expression: &Expression) -> bool {
        match expression {
            Expression::GlobalVariable(handle) => self.globals.contains(handle),
            Expression::FunctionArgument(index) => self.arguments[*index as usize],
            Expression::LocalVariable(handle) => self.locals.contains(handle),
            Expression::Access { base, .. }
            | Expression::AccessIndex { base, .. }
            | Expression::Load { pointer: base } => self.oned[base.index()],
            _ => false,
        }
    }

    fn pair(
        &self,
        first: Handle<Expression>,
        second: Literal,
        ty: Handle<Type>,
        span: Span,
        expressions: &mut Arena<Expression>,
    ) -> Handle<Expression> {
        let second = expressions.append(Expression::Literal(second), span);
        expressions.append(
            Expression::Compose {
                ty,
                components: vec![first, second],
            },
            span,
        )
    }

    fn expression(
        &self,
        mut expression: Expression,
        span: Span,
        expressions: &mut Arena<Expression>,
    ) -> Handle<Expression> {
        let original = expression.clone();
        remap::expression(&self.map, &mut expression);
        match original {
            Expression::ImageSample { image, .. } if self.oned[image.index()] => {
                let Expression::ImageSample {
                    coordinate,
                    offset,
                    level,
                    ..
                } = &mut expression
                else {
                    unreachable!()
                };
                *coordinate = self.pair(
                    *coordinate,
                    Literal::F32(0.5),
                    self.float2,
                    span,
                    expressions,
                );
                if let Some(value) = offset {
                    *value = self.pair(*value, Literal::I32(0), self.sint2, span, expressions);
                }
                if let naga::SampleLevel::Gradient { x, y } = level {
                    *x = self.pair(*x, Literal::F32(0.0), self.float2, span, expressions);
                    *y = self.pair(*y, Literal::F32(0.0), self.float2, span, expressions);
                }
                expressions.append(expression, span)
            }
            Expression::ImageLoad { image, .. } if self.oned[image.index()] => {
                let Expression::ImageLoad { coordinate, .. } = &mut expression else {
                    unreachable!()
                };
                *coordinate =
                    self.pair(*coordinate, Literal::I32(0), self.sint2, span, expressions);
                expressions.append(expression, span)
            }
            Expression::ImageQuery { image, query } if self.oned[image.index()] => {
                let queried = expressions.append(expression, span);
                if matches!(query, naga::ImageQuery::Size { .. }) {
                    expressions.append(
                        Expression::AccessIndex {
                            base: queried,
                            index: 0,
                        },
                        span,
                    )
                } else {
                    queried
                }
            }
            _ => expressions.append(expression, span),
        }
    }

    fn block(&self, block: &mut naga::Block, expressions: &mut Arena<Expression>) {
        let mut rebuilt = naga::Block::with_capacity(block.len());
        for (mut statement, span) in mem::take(block).span_into_iter() {
            match &mut statement {
                Statement::ImageStore {
                    image,
                    coordinate,
                    array_index,
                    value,
                } if self.oned[image.index()] => {
                    *image = self.map[image.index()];
                    let first = expressions.append(Expression::Literal(Literal::I32(0)), span);
                    *coordinate = expressions.append(
                        Expression::Compose {
                            ty: self.sint2,
                            components: vec![self.map[coordinate.index()], first],
                        },
                        span,
                    );
                    if let Some(index) = array_index {
                        *index = self.map[index.index()];
                    }
                    *value = self.map[value.index()];
                    rebuilt.push(
                        Statement::Emit(naga::Range::new_from_bounds(first, *coordinate)),
                        span,
                    );
                    rebuilt.push(statement, span);
                }
                Statement::ImageAtomic {
                    image,
                    coordinate,
                    array_index,
                    fun,
                    value,
                    result,
                } if self.oned[image.index()] => {
                    *image = self.map[image.index()];
                    let second = expressions.append(Expression::Literal(Literal::I32(0)), span);
                    *coordinate = expressions.append(
                        Expression::Compose {
                            ty: self.sint2,
                            components: vec![self.map[coordinate.index()], second],
                        },
                        span,
                    );
                    if let Some(index) = array_index {
                        *index = self.map[index.index()];
                    }
                    if let naga::AtomicFunction::Exchange {
                        compare: Some(compare),
                    } = fun
                    {
                        *compare = self.map[compare.index()];
                    }
                    *value = self.map[value.index()];
                    if let Some(result) = result {
                        *result = self.map[result.index()];
                    }
                    rebuilt.push(
                        Statement::Emit(naga::Range::new_from_bounds(second, *coordinate)),
                        span,
                    );
                    rebuilt.push(statement, span);
                }
                Statement::Block(nested) => {
                    self.block(nested, expressions);
                    rebuilt.push(statement, span);
                }
                Statement::If {
                    condition,
                    accept,
                    reject,
                } => {
                    *condition = self.map[condition.index()];
                    self.block(accept, expressions);
                    self.block(reject, expressions);
                    rebuilt.push(statement, span);
                }
                Statement::Switch { selector, cases } => {
                    *selector = self.map[selector.index()];
                    for case in cases {
                        self.block(&mut case.body, expressions);
                    }
                    rebuilt.push(statement, span);
                }
                Statement::Loop {
                    body,
                    continuing,
                    break_if,
                } => {
                    if let Some(condition) = break_if {
                        *condition = self.map[condition.index()];
                    }
                    self.block(body, expressions);
                    self.block(continuing, expressions);
                    rebuilt.push(statement, span);
                }
                _ => {
                    remap::statement(&self.map, &self.spans, &mut statement);
                    rebuilt.push(statement, span);
                }
            }
        }
        *block = rebuilt;
    }
}
