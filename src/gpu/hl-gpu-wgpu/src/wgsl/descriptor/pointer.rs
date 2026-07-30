use super::*;

impl FunctionLowering<'_> {
    pub(super) fn access(
        &self,
        pointer: &Pointer,
        dynamic: Option<Handle<Expression>>,
        constant: Option<u32>,
        span: Span,
        expressions: &mut Arena<Expression>,
    ) -> Result<(Handle<Expression>, Option<Pointer>)> {
        match pointer {
            Pointer::Array { globals, ty } => {
                let handles = globals
                    .iter()
                    .map(|global| expressions.append(Expression::GlobalVariable(*global), span))
                    .collect::<Vec<_>>();
                if let Some(index) = constant {
                    return handles
                        .get(index as usize)
                        .copied()
                        .map(|handle| (handle, None))
                        .ok_or(GpuError::OutOfBounds);
                }
                Ok((
                    handles[0],
                    Some(Pointer::Selected {
                        pointers: handles,
                        selector: dynamic.unwrap(),
                        ty: *ty,
                    }),
                ))
            }
            Pointer::Selected {
                pointers,
                selector,
                ty,
            } => {
                let child = match &self.types[*ty].inner {
                    TypeInner::Struct { members, .. } => {
                        members
                            .get(constant.ok_or(GpuError::Invalid(
                                "struct member requires a constant index",
                            ))? as usize)
                            .map(|member| member.ty)
                            .ok_or(GpuError::OutOfBounds)?
                    }
                    TypeInner::Array { base, .. } | TypeInner::BindingArray { base, .. } => *base,
                    TypeInner::Vector { scalar, .. } => self
                        .types
                        .iter()
                        .find_map(|(handle, ty)| {
                            matches!(ty.inner, TypeInner::Scalar(candidate) if candidate == *scalar)
                                .then_some(handle)
                        })
                        .ok_or(GpuError::Invalid("descriptor vector scalar type is absent"))?,
                    TypeInner::Matrix { rows, scalar, .. } => self
                        .types
                        .iter()
                        .find_map(|(handle, ty)| {
                            matches!(
                                ty.inner,
                                TypeInner::Vector {
                                    size,
                                    scalar: candidate,
                                } if size == *rows && candidate == *scalar
                            )
                            .then_some(handle)
                        })
                        .ok_or(GpuError::Invalid("descriptor matrix column type is absent"))?,
                    _ => {
                        return Err(GpuError::Unsupported(
                            "descriptor array pointer shape is unsupported",
                        ))
                    }
                };
                let handles = pointers
                    .iter()
                    .map(|base| {
                        expressions.append(
                            match (dynamic, constant) {
                                (Some(index), None) => Expression::Access { base: *base, index },
                                (None, Some(index)) => {
                                    Expression::AccessIndex { base: *base, index }
                                }
                                _ => unreachable!(),
                            },
                            span,
                        )
                    })
                    .collect::<Vec<_>>();
                Ok((
                    handles[0],
                    Some(Pointer::Selected {
                        pointers: handles,
                        selector: *selector,
                        ty: child,
                    }),
                ))
            }
        }
    }
}
