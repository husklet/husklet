use super::*;

impl FunctionLowering<'_> {
    pub(super) fn selected_value(
        &self,
        pointers: &[Handle<Expression>],
        selector: Handle<Expression>,
        ty: Handle<Type>,
        span: Span,
        expressions: &mut Arena<Expression>,
    ) -> Result<Handle<Expression>> {
        match &self.types[ty].inner {
            TypeInner::Scalar(_) | TypeInner::Vector { .. } => {
                let mut selected = expressions.append(Expression::ZeroValue(ty), span);
                for (element, pointer) in pointers.iter().enumerate() {
                    let value = expressions.append(Expression::Load { pointer: *pointer }, span);
                    let literal =
                        expressions.append(Expression::Literal(Literal::U32(element as u32)), span);
                    let condition = expressions.append(
                        Expression::Binary {
                            op: BinaryOperator::Equal,
                            left: selector,
                            right: literal,
                        },
                        span,
                    );
                    selected = expressions.append(
                        Expression::Select {
                            condition,
                            accept: value,
                            reject: selected,
                        },
                        span,
                    );
                }
                Ok(selected)
            }
            TypeInner::Struct { members, .. } => {
                let mut components = Vec::with_capacity(members.len());
                for (index, member) in members.iter().enumerate() {
                    let children = pointers
                        .iter()
                        .map(|base| {
                            expressions.append(
                                Expression::AccessIndex {
                                    base: *base,
                                    index: index as u32,
                                },
                                span,
                            )
                        })
                        .collect::<Vec<_>>();
                    components.push(self.selected_value(
                        &children,
                        selector,
                        member.ty,
                        span,
                        expressions,
                    )?);
                }
                Ok(expressions.append(Expression::Compose { ty, components }, span))
            }
            TypeInner::Array { base, size, .. } => {
                let naga::ArraySize::Constant(count) = size else {
                    return Err(GpuError::Unsupported(
                        "runtime-sized buffer descriptor values are unsupported",
                    ));
                };
                let mut components = Vec::with_capacity(count.get() as usize);
                for index in 0..count.get() {
                    let children = pointers
                        .iter()
                        .map(|pointer| {
                            expressions.append(
                                Expression::AccessIndex {
                                    base: *pointer,
                                    index,
                                },
                                span,
                            )
                        })
                        .collect::<Vec<_>>();
                    components.push(self.selected_value(
                        &children,
                        selector,
                        *base,
                        span,
                        expressions,
                    )?);
                }
                Ok(expressions.append(Expression::Compose { ty, components }, span))
            }
            TypeInner::Matrix {
                columns,
                rows,
                scalar,
            } => {
                let column = self
                    .types
                    .iter()
                    .find_map(|(handle, candidate)| {
                        matches!(
                            candidate.inner,
                            TypeInner::Vector {
                                size,
                                scalar: candidate_scalar,
                            } if size == *rows && candidate_scalar == *scalar
                        )
                        .then_some(handle)
                    })
                    .ok_or(GpuError::Invalid("uniform matrix column type is absent"))?;
                let mut components = Vec::with_capacity(*columns as usize);
                for index in 0..*columns as u32 {
                    let children = pointers
                        .iter()
                        .map(|pointer| {
                            expressions.append(
                                Expression::AccessIndex {
                                    base: *pointer,
                                    index,
                                },
                                span,
                            )
                        })
                        .collect::<Vec<_>>();
                    components.push(self.selected_value(
                        &children,
                        selector,
                        column,
                        span,
                        expressions,
                    )?);
                }
                Ok(expressions.append(Expression::Compose { ty, components }, span))
            }
            _ => Err(GpuError::Unsupported(
                "buffer descriptor array value type is unsupported",
            )),
        }
    }
}
