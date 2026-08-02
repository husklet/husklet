use super::*;
use naga::{Block, Range, Statement, SwitchCase, SwitchValue};

impl FunctionLowering<'_> {
    pub(super) fn block(
        &self,
        block: &mut Block,
        expressions: &mut Arena<Expression>,
    ) -> Result<()> {
        let mut rebuilt = Block::with_capacity(block.len());
        for (statement, span) in mem::take(block).span_into_iter() {
            match statement {
                Statement::Block(mut nested) => {
                    self.block(&mut nested, expressions)?;
                    rebuilt.push(Statement::Block(nested), span);
                }
                Statement::If {
                    condition,
                    mut accept,
                    mut reject,
                } => {
                    self.block(&mut accept, expressions)?;
                    self.block(&mut reject, expressions)?;
                    rebuilt.push(
                        Statement::If {
                            condition: self.map[condition.index()],
                            accept,
                            reject,
                        },
                        span,
                    );
                }
                Statement::Switch {
                    selector,
                    mut cases,
                } => {
                    for case in &mut cases {
                        self.block(&mut case.body, expressions)?;
                    }
                    rebuilt.push(
                        Statement::Switch {
                            selector: self.map[selector.index()],
                            cases,
                        },
                        span,
                    );
                }
                Statement::Loop {
                    mut body,
                    mut continuing,
                    break_if,
                } => {
                    self.block(&mut body, expressions)?;
                    self.block(&mut continuing, expressions)?;
                    rebuilt.push(
                        Statement::Loop {
                            body,
                            continuing,
                            break_if: break_if.map(|value| self.map[value.index()]),
                        },
                        span,
                    );
                }
                Statement::Store { pointer, value } => {
                    let value = self.map[value.index()];
                    if let Some(Pointer::Selected {
                        pointers, selector, ..
                    }) = self.pointers[pointer.index()].as_ref()
                    {
                        rebuilt.push(
                            Statement::Switch {
                                selector: *selector,
                                cases: Self::cases(pointers, |pointer| Statement::Store {
                                    pointer,
                                    value,
                                }),
                            },
                            span,
                        );
                    } else {
                        rebuilt.push(
                            Statement::Store {
                                pointer: self.map[pointer.index()],
                                value,
                            },
                            span,
                        );
                    }
                }
                Statement::ImageStore {
                    image,
                    coordinate,
                    array_index,
                    value,
                } => {
                    if let Some(Pointer::Selected {
                        pointers, selector, ..
                    }) = self.pointers[image.index()].as_ref()
                    {
                        let coordinate = self.map[coordinate.index()];
                        let array_index = array_index.map(|value| self.map[value.index()]);
                        let value = self.map[value.index()];
                        rebuilt.push(
                            Statement::Switch {
                                selector: *selector,
                                cases: Self::cases(pointers, |image| Statement::ImageStore {
                                    image,
                                    coordinate,
                                    array_index,
                                    value,
                                }),
                            },
                            span,
                        );
                    } else {
                        let mut nested = Block::from_vec(vec![Statement::ImageStore {
                            image,
                            coordinate,
                            array_index,
                            value,
                        }]);
                        remap_block(&self.map, &self.spans, &mut nested);
                        for (statement, span) in nested.span_into_iter() {
                            rebuilt.push(statement, span);
                        }
                    }
                }
                Statement::ImageAtomic {
                    image,
                    coordinate,
                    array_index,
                    fun,
                    value,
                    result,
                } => {
                    if let Some(Pointer::Selected {
                        pointers, selector, ..
                    }) = self.pointers[image.index()].as_ref()
                    {
                        let coordinate = self.map[coordinate.index()];
                        let array_index = array_index.map(|value| self.map[value.index()]);
                        let value = self.map[value.index()];
                        let result = result.map(|value| self.map[value.index()]);
                        rebuilt.push(
                            Statement::Switch {
                                selector: *selector,
                                cases: Self::cases(pointers, |image| Statement::ImageAtomic {
                                    image,
                                    coordinate,
                                    array_index,
                                    fun,
                                    value,
                                    result,
                                }),
                            },
                            span,
                        );
                    } else {
                        let mut nested = Block::from_vec(vec![Statement::ImageAtomic {
                            image,
                            coordinate,
                            array_index,
                            fun,
                            value,
                            result,
                        }]);
                        remap_block(&self.map, &self.spans, &mut nested);
                        for (statement, span) in nested.span_into_iter() {
                            rebuilt.push(statement, span);
                        }
                    }
                }
                Statement::Atomic {
                    pointer,
                    fun,
                    value,
                    result,
                } => {
                    let pointers = match self.pointers[pointer.index()].as_ref() {
                        Some(Pointer::Selected { pointers, .. }) => pointers.clone(),
                        _ => vec![self.map[pointer.index()]],
                    };
                    let selector = match self.pointers[pointer.index()].as_ref() {
                        Some(Pointer::Selected { selector, .. }) => Some(*selector),
                        _ => None,
                    };
                    let value = self.map[value.index()];
                    let fun = match fun {
                        naga::AtomicFunction::Exchange {
                            compare: Some(compare),
                        } => naga::AtomicFunction::Exchange {
                            compare: Some(self.map[compare.index()]),
                        },
                        fun => fun,
                    };
                    let atomic = result.and_then(|result| self.atomic_results[result.index()]);
                    let mut branch = |pointer| {
                        let mut body = Block::new();
                        let branch_result = atomic.map(|result| {
                            expressions.append(
                                Expression::AtomicResult {
                                    ty: result.ty,
                                    comparison: result.comparison,
                                },
                                span,
                            )
                        });
                        body.push(
                            Statement::Atomic {
                                pointer,
                                fun,
                                value,
                                result: branch_result,
                            },
                            span,
                        );
                        if let (Some(result), Some(value)) = (atomic, branch_result) {
                            body.push(
                                Statement::Store {
                                    pointer: result.local,
                                    value,
                                },
                                span,
                            );
                        }
                        body
                    };
                    if let Some(selector) = selector {
                        let mut cases = pointers
                            .into_iter()
                            .enumerate()
                            .map(|(index, pointer)| SwitchCase {
                                value: SwitchValue::U32(index as u32),
                                body: branch(pointer),
                                fall_through: false,
                            })
                            .collect::<Vec<_>>();
                        cases.push(SwitchCase {
                            value: SwitchValue::Default,
                            body: Block::new(),
                            fall_through: false,
                        });
                        rebuilt.push(Statement::Switch { selector, cases }, span);
                    } else {
                        for (statement, span) in branch(pointers[0]).span_into_iter() {
                            rebuilt.push(statement, span);
                        }
                    }
                    if let Some(result) = result {
                        let mapped = self.map[result.index()];
                        rebuilt.push(
                            Statement::Emit(Range::new_from_bounds(mapped, mapped)),
                            span,
                        );
                    }
                }
                Statement::Call { ref arguments, .. }
                    if arguments.iter().any(|argument| {
                        matches!(
                            self.pointers[argument.index()],
                            Some(Pointer::Selected { .. })
                        )
                    }) =>
                {
                    return Err(GpuError::Unsupported(
                        "dynamic descriptor pointers passed to functions are unsupported",
                    ));
                }
                Statement::Return { value: Some(value) }
                    if matches!(self.pointers[value.index()], Some(Pointer::Selected { .. })) =>
                {
                    return Err(GpuError::Unsupported(
                        "returning a dynamic descriptor pointer is unsupported",
                    ));
                }
                Statement::WorkGroupUniformLoad { pointer, .. }
                    if matches!(
                        self.pointers[pointer.index()],
                        Some(Pointer::Selected { .. })
                    ) =>
                {
                    return Err(GpuError::Unsupported(
                        "workgroup loads from dynamic descriptor pointers are unsupported",
                    ));
                }
                statement => {
                    let mut nested = Block::from_vec(vec![statement]);
                    remap_block(&self.map, &self.spans, &mut nested);
                    for (statement, span) in nested.span_into_iter() {
                        rebuilt.push(statement, span);
                    }
                }
            }
        }
        *block = rebuilt;
        Ok(())
    }

    fn cases(
        pointers: &[Handle<Expression>],
        statement: impl Fn(Handle<Expression>) -> Statement,
    ) -> Vec<SwitchCase> {
        let mut cases = pointers
            .iter()
            .copied()
            .enumerate()
            .map(|(index, pointer)| SwitchCase {
                value: SwitchValue::U32(index as u32),
                body: Block::from_vec(vec![statement(pointer)]),
                fall_through: false,
            })
            .collect::<Vec<_>>();
        cases.push(SwitchCase {
            value: SwitchValue::Default,
            body: Block::new(),
            fall_through: false,
        });
        cases
    }
}
