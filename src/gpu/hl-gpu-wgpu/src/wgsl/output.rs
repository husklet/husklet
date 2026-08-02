//! Vulkan fragment-output compatibility that WebGPU's stricter pipeline interface cannot express directly.

use std::mem;

use hl_gpu::protocol::model::descriptor::ColorTargetState;
use hl_gpu::protocol::model::enums::TextureNumericClass;
use hl_gpu::{GpuError, Result};
use naga::{Binding, Block, Expression, Handle, ScalarKind, Span, Statement, Type, TypeInner};

pub(super) struct FragmentOutputs;

struct Conversion {
    result_ty: Handle<Type>,
    members: Option<Vec<Option<(ScalarKind, u8)>>>,
    scalar: Option<(ScalarKind, u8)>,
}

impl FragmentOutputs {
    /// Convert only the Vulkan-permitted Sint↔Uint difference at the selected fragment entry point.
    pub(super) fn adapt(
        module: &mut naga::Module,
        entry_name: &str,
        targets: &[ColorTargetState],
    ) -> Result<bool> {
        let entry_index = module
            .entry_points
            .iter()
            .position(|entry| {
                entry.stage == naga::ShaderStage::Fragment && entry.name == entry_name
            })
            .ok_or_else(|| {
                GpuError::Kernel(format!(
                    "wgpu: fragment entry {entry_name:?} absent during output specialization"
                ))
            })?;
        let Some(result) = module.entry_points[entry_index].function.result.clone() else {
            return Ok(false);
        };
        let result_name = module.types[result.ty].name.clone();
        let result_inner = module.types[result.ty].inner.clone();

        let conversion = match result_inner {
            TypeInner::Struct { members, span } => {
                let mut rebuilt = members.clone();
                let mut conversions = Vec::with_capacity(members.len());
                let mut changed = false;
                for (old, new) in members.iter().zip(&mut rebuilt) {
                    let desired = Self::desired_kind(old.binding.as_ref(), targets);
                    let adapted = desired
                        .map(|kind| Self::adapt_type(&mut module.types, old.ty, kind))
                        .transpose()?
                        .flatten();
                    if let Some((ty, kind, width)) = adapted {
                        new.ty = ty;
                        conversions.push(Some((kind, width)));
                        changed = true;
                    } else {
                        conversions.push(None);
                    }
                }
                if !changed {
                    return Ok(false);
                }
                let result_ty = module.types.insert(
                    Type {
                        name: result_name,
                        inner: TypeInner::Struct {
                            members: rebuilt,
                            span,
                        },
                    },
                    Span::default(),
                );
                Conversion {
                    result_ty,
                    members: Some(conversions),
                    scalar: None,
                }
            }
            _ => {
                let Some(desired) = Self::desired_kind(result.binding.as_ref(), targets) else {
                    return Ok(false);
                };
                let Some((result_ty, kind, width)) =
                    Self::adapt_type(&mut module.types, result.ty, desired)?
                else {
                    return Ok(false);
                };
                Conversion {
                    result_ty,
                    members: None,
                    scalar: Some((kind, width)),
                }
            }
        };

        let function = &mut module.entry_points[entry_index].function;
        function.result.as_mut().expect("result established").ty = conversion.result_ty;
        let mut body = mem::take(&mut function.body);
        Self::rewrite_block(&mut body, &mut function.expressions, &conversion);
        function.body = body;
        Ok(true)
    }

    fn desired_kind(binding: Option<&Binding>, targets: &[ColorTargetState]) -> Option<ScalarKind> {
        let Binding::Location { location, .. } = binding? else {
            return None;
        };
        match targets.get(*location as usize)?.format.numeric_class() {
            TextureNumericClass::Uint => Some(ScalarKind::Uint),
            TextureNumericClass::Sint => Some(ScalarKind::Sint),
            TextureNumericClass::Float => None,
        }
    }

    fn adapt_type(
        types: &mut naga::UniqueArena<Type>,
        original: Handle<Type>,
        desired: ScalarKind,
    ) -> Result<Option<(Handle<Type>, ScalarKind, u8)>> {
        let replacement =
            match types[original].inner {
                TypeInner::Scalar(scalar)
                    if matches!(scalar.kind, ScalarKind::Sint | ScalarKind::Uint)
                        && scalar.kind != desired =>
                {
                    TypeInner::Scalar(naga::Scalar {
                        kind: desired,
                        width: scalar.width,
                    })
                }
                TypeInner::Vector { size, scalar }
                    if matches!(scalar.kind, ScalarKind::Sint | ScalarKind::Uint)
                        && scalar.kind != desired =>
                {
                    TypeInner::Vector {
                        size,
                        scalar: naga::Scalar {
                            kind: desired,
                            width: scalar.width,
                        },
                    }
                }
                TypeInner::Scalar(scalar) if scalar.kind == desired => return Ok(None),
                TypeInner::Vector { scalar, .. } if scalar.kind == desired => return Ok(None),
                TypeInner::Scalar(_) | TypeInner::Vector { .. } => return Ok(None),
                _ => return Err(GpuError::Unsupported(
                    "wgpu: Vulkan signedness conversion requires scalar or vector integer output",
                )),
            };
        let width = match replacement {
            TypeInner::Scalar(scalar) | TypeInner::Vector { scalar, .. } => scalar.width,
            _ => unreachable!(),
        };
        let ty = types.insert(
            Type {
                name: None,
                inner: replacement,
            },
            Span::default(),
        );
        Ok(Some((ty, desired, width)))
    }

    fn rewrite_block(
        block: &mut Block,
        expressions: &mut naga::Arena<Expression>,
        conversion: &Conversion,
    ) {
        let mut rebuilt = Block::with_capacity(block.len());
        for (mut statement, span) in mem::take(block).span_into_iter() {
            match &mut statement {
                Statement::Block(nested) => Self::rewrite_block(nested, expressions, conversion),
                Statement::If { accept, reject, .. } => {
                    Self::rewrite_block(accept, expressions, conversion);
                    Self::rewrite_block(reject, expressions, conversion);
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        Self::rewrite_block(&mut case.body, expressions, conversion);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    Self::rewrite_block(body, expressions, conversion);
                    Self::rewrite_block(continuing, expressions, conversion);
                }
                Statement::Return { value: Some(value) } => {
                    let old = *value;
                    let mut first = None;
                    let converted = if let Some(members) = &conversion.members {
                        let mut components = Vec::with_capacity(members.len());
                        for (index, member) in members.iter().enumerate() {
                            let field = expressions.append(
                                Expression::AccessIndex {
                                    base: old,
                                    index: index as u32,
                                },
                                span,
                            );
                            first.get_or_insert(field);
                            components.push(if let Some((kind, width)) = member {
                                expressions.append(
                                    Expression::As {
                                        expr: field,
                                        kind: *kind,
                                        convert: Some(*width),
                                    },
                                    span,
                                )
                            } else {
                                field
                            });
                        }
                        expressions.append(
                            Expression::Compose {
                                ty: conversion.result_ty,
                                components,
                            },
                            span,
                        )
                    } else {
                        let (kind, width) = conversion.scalar.expect("scalar conversion");
                        let cast = expressions.append(
                            Expression::As {
                                expr: old,
                                kind,
                                convert: Some(width),
                            },
                            span,
                        );
                        first = Some(cast);
                        cast
                    };
                    rebuilt.push(
                        Statement::Emit(naga::Range::new_from_bounds(
                            first.expect("fragment result has at least one member"),
                            converted,
                        )),
                        span,
                    );
                    *value = converted;
                }
                _ => {}
            }
            rebuilt.push(statement, span);
        }
        *block = rebuilt;
    }
}
