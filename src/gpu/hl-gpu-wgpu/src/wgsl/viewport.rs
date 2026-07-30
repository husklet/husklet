//! Host resources and vertex correction needed to preserve GL viewport semantics on wgpu.

use hl_gpu::{GpuError, Result};
use naga::{
    AddressSpace, BinaryOperator, Binding, Block, BuiltIn, Expression, Function, GlobalVariable,
    ResourceBinding, Scalar, ScalarKind, ShaderStage, Span, Statement, Type, TypeInner, VectorSize,
};

/// Binding zero is host-owned. Guest group-zero resources are shifted by one during lowering.
pub(crate) const BINDING: u32 = 0;
pub(crate) const GUEST_OFFSET: u32 = 1;

pub(crate) struct Shader;

impl Shader {
    pub(crate) fn prepare(module: &mut naga::Module) -> Result<()> {
        for (_, variable) in module.global_variables.iter_mut() {
            if let Some(binding) = variable.binding.as_mut() {
                if binding.group == 0 {
                    binding.binding = binding
                        .binding
                        .checked_add(GUEST_OFFSET)
                        .ok_or(GpuError::OutOfBounds)?;
                }
            }
        }

        let vertex_outputs = module
            .entry_points
            .iter()
            .map(|entry| {
                (entry.stage == ShaderStage::Vertex)
                    .then(|| Output::from_function(module, &entry.function))
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        if vertex_outputs.iter().all(Option::is_none) {
            return Ok(());
        }

        let vec4 = module.types.insert(
            Type {
                name: Some("HlViewport".into()),
                inner: TypeInner::Vector {
                    size: VectorSize::Quad,
                    scalar: Scalar {
                        kind: ScalarKind::Float,
                        width: 4,
                    },
                },
            },
            Span::default(),
        );
        let uniform = module.global_variables.append(
            GlobalVariable {
                name: Some("hl_viewport".into()),
                space: AddressSpace::Uniform,
                binding: Some(ResourceBinding {
                    group: 0,
                    binding: BINDING,
                }),
                ty: vec4,
                init: None,
            },
            Span::default(),
        );

        for (entry, output) in module.entry_points.iter_mut().zip(vertex_outputs) {
            if let Some(output) = output {
                output.rewrite(&mut entry.function, uniform);
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
enum Output {
    Position {
        ty: naga::Handle<Type>,
    },
    Struct {
        ty: naga::Handle<Type>,
        position_ty: naga::Handle<Type>,
        position: usize,
        members: usize,
    },
}

impl Output {
    fn from_function(module: &naga::Module, function: &Function) -> Result<Self> {
        let result = function
            .result
            .as_ref()
            .ok_or(GpuError::Invalid("vertex shader has no position result"))?;
        if matches!(
            result.binding,
            Some(Binding::BuiltIn(BuiltIn::Position { .. }))
        ) {
            return Ok(Self::Position { ty: result.ty });
        }
        let TypeInner::Struct { members, .. } = &module.types[result.ty].inner else {
            return Err(GpuError::Invalid(
                "vertex shader result does not expose a position",
            ));
        };
        let position = members
            .iter()
            .position(|member| {
                matches!(
                    member.binding,
                    Some(Binding::BuiltIn(BuiltIn::Position { .. }))
                )
            })
            .ok_or(GpuError::Invalid(
                "vertex shader result does not expose a position",
            ))?;
        Ok(Self::Struct {
            ty: result.ty,
            position_ty: members[position].ty,
            position,
            members: members.len(),
        })
    }

    fn rewrite(&self, function: &mut Function, uniform: naga::Handle<GlobalVariable>) {
        let expressions = &mut function.expressions;
        Self::rewrite_block(&mut function.body, expressions, uniform, self);
    }

    fn rewrite_block(
        block: &mut Block,
        expressions: &mut naga::Arena<Expression>,
        uniform: naga::Handle<GlobalVariable>,
        output: &Self,
    ) {
        let mut rewritten = Block::with_capacity(block.len());
        for (mut statement, span) in std::mem::take(block).span_into_iter() {
            match &mut statement {
                Statement::Block(child) => Self::rewrite_block(child, expressions, uniform, output),
                Statement::If { accept, reject, .. } => {
                    Self::rewrite_block(accept, expressions, uniform, output);
                    Self::rewrite_block(reject, expressions, uniform, output);
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        Self::rewrite_block(&mut case.body, expressions, uniform, output);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    Self::rewrite_block(body, expressions, uniform, output);
                    Self::rewrite_block(continuing, expressions, uniform, output);
                }
                Statement::Return { value: Some(value) } => {
                    let uniform =
                        expressions.append(Expression::GlobalVariable(uniform), Span::default());
                    let first =
                        expressions.append(Expression::Load { pointer: uniform }, Span::default());
                    let loaded = first;
                    let position = match output {
                        Self::Position { .. } => *value,
                        Self::Struct { position, .. } => expressions.append(
                            Expression::AccessIndex {
                                base: *value,
                                index: *position as u32,
                            },
                            Span::default(),
                        ),
                    };
                    let position_ty = match output {
                        Self::Position { ty } => *ty,
                        Self::Struct { position_ty, .. } => *position_ty,
                    };
                    let corrected = Self::correct(expressions, position_ty, position, loaded);
                    let replacement = match output {
                        Self::Position { .. } => corrected,
                        Self::Struct {
                            ty,
                            position,
                            members,
                            ..
                        } => {
                            let components = (0..*members)
                                .map(|index| {
                                    if index == *position {
                                        corrected
                                    } else {
                                        expressions.append(
                                            Expression::AccessIndex {
                                                base: *value,
                                                index: index as u32,
                                            },
                                            Span::default(),
                                        )
                                    }
                                })
                                .collect();
                            expressions.append(
                                Expression::Compose {
                                    ty: *ty,
                                    components,
                                },
                                Span::default(),
                            )
                        }
                    };
                    let last = replacement;
                    rewritten.push(
                        Statement::Emit(naga::Range::new_from_bounds(first, last)),
                        Span::default(),
                    );
                    *value = replacement;
                }
                _ => {}
            }
            rewritten.push(statement, span);
        }
        *block = rewritten;
    }

    fn correct(
        expressions: &mut naga::Arena<Expression>,
        position_ty: naga::Handle<Type>,
        position: naga::Handle<Expression>,
        uniform: naga::Handle<Expression>,
    ) -> naga::Handle<Expression> {
        let component =
            |expressions: &mut naga::Arena<Expression>, base: naga::Handle<Expression>, index| {
                expressions.append(Expression::AccessIndex { base, index }, Span::default())
            };
        let px = component(expressions, position, 0);
        let py = component(expressions, position, 1);
        let pz = component(expressions, position, 2);
        let pw = component(expressions, position, 3);
        let sx = component(expressions, uniform, 0);
        let sy = component(expressions, uniform, 1);
        let bx = component(expressions, uniform, 2);
        let by = component(expressions, uniform, 3);
        let sx_px = expressions.append(
            Expression::Binary {
                op: BinaryOperator::Multiply,
                left: sx,
                right: px,
            },
            Span::default(),
        );
        let bx_pw = expressions.append(
            Expression::Binary {
                op: BinaryOperator::Multiply,
                left: bx,
                right: pw,
            },
            Span::default(),
        );
        let x = expressions.append(
            Expression::Binary {
                op: BinaryOperator::Add,
                left: sx_px,
                right: bx_pw,
            },
            Span::default(),
        );
        let sy_py = expressions.append(
            Expression::Binary {
                op: BinaryOperator::Multiply,
                left: sy,
                right: py,
            },
            Span::default(),
        );
        let by_pw = expressions.append(
            Expression::Binary {
                op: BinaryOperator::Multiply,
                left: by,
                right: pw,
            },
            Span::default(),
        );
        let y = expressions.append(
            Expression::Binary {
                op: BinaryOperator::Add,
                left: sy_py,
                right: by_pw,
            },
            Span::default(),
        );
        expressions.append(
            Expression::Compose {
                ty: position_ty,
                components: vec![x, y, pz, pw],
            },
            Span::default(),
        )
    }
}
