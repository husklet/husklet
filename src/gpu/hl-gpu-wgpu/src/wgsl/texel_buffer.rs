//! Vulkan texel-buffer images lowered to sampler-less typed storage arrays.

use std::mem;

use hl_gpu::{GpuError, Result};
use naga::{Arena, Expression, Handle, Span, Statement, Type, TypeInner};

use super::descriptor::remap;

mod raw;

pub(super) struct TexelBuffers;

impl TexelBuffers {
    pub(super) fn lower(
        module: &mut naga::Module,
        specialization: Option<&[crate::texel_buffer::Specialization]>,
    ) -> Result<()> {
        if let Some(specialization) = specialization {
            return raw::lower(module, specialization);
        }
        let buffers = module
            .global_variables
            .iter()
            .filter_map(|(handle, variable)| {
                let TypeInner::Image {
                    dim: naga::ImageDimension::Buffer,
                    arrayed,
                    class,
                } = module.types[variable.ty].inner
                else {
                    return None;
                };
                Some((handle, arrayed, class))
            })
            .collect::<Vec<_>>();
        if buffers.is_empty() {
            return Ok(());
        }

        for (handle, arrayed, class) in &buffers {
            if *arrayed {
                return Err(GpuError::Unsupported("arrayed texel buffers"));
            }
            let atomic = matches!(class, naga::ImageClass::Storage { access, .. } if access.contains(naga::StorageAccess::ATOMIC));
            let (kind, access) = image_class(*class)?;
            let scalar = naga::Scalar { kind, width: 4 };
            let element = module.types.insert(
                Type {
                    name: None,
                    inner: if atomic {
                        TypeInner::Atomic(scalar)
                    } else {
                        TypeInner::Vector {
                            size: naga::VectorSize::Quad,
                            scalar,
                        }
                    },
                },
                Span::default(),
            );
            let array = module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Array {
                        base: element,
                        size: naga::ArraySize::Dynamic,
                        stride: 16,
                    },
                },
                Span::default(),
            );
            let structure = module.types.insert(
                Type {
                    name: Some("_hl_texel_buffer".into()),
                    inner: TypeInner::Struct {
                        members: vec![naga::StructMember {
                            name: Some("texels".into()),
                            ty: array,
                            binding: None,
                            offset: 0,
                        }],
                        span: 16,
                    },
                },
                Span::default(),
            );
            let variable = &mut module.global_variables[*handle];
            variable.ty = structure;
            variable.space = naga::AddressSpace::Storage { access };
        }

        let handles = buffers.iter().map(|(h, _, _)| *h).collect::<Vec<_>>();
        for (_, function) in module.functions.iter_mut() {
            FunctionLowering::new(&handles).lower(function)?;
        }
        for entry in &mut module.entry_points {
            FunctionLowering::new(&handles).lower(&mut entry.function)?;
        }
        Ok(())
    }
}

fn image_class(class: naga::ImageClass) -> Result<(naga::ScalarKind, naga::StorageAccess)> {
    use naga::{ImageClass, ScalarKind, StorageAccess, StorageFormat as F};
    match class {
        ImageClass::Sampled { kind, multi: false } => Ok((kind, StorageAccess::LOAD)),
        ImageClass::Storage { format, access } => {
            let kind = match format {
                F::R8Uint
                | F::R16Uint
                | F::R32Uint
                | F::Rg8Uint
                | F::Rg16Uint
                | F::Rg32Uint
                | F::Rgba8Uint
                | F::Rgba16Uint
                | F::Rgba32Uint
                | F::Rgb10a2Uint
                | F::R64Uint => ScalarKind::Uint,
                F::R8Sint
                | F::R16Sint
                | F::R32Sint
                | F::Rg8Sint
                | F::Rg16Sint
                | F::Rg32Sint
                | F::Rgba8Sint
                | F::Rgba16Sint
                | F::Rgba32Sint => ScalarKind::Sint,
                _ => ScalarKind::Float,
            };
            // WGSL has read-only and read_write storage buffers, but no write-only address space.
            // A Vulkan storage texel buffer declared write-only therefore still needs LOAD in the host
            // representation; the shader continues to contain no load unless SPIR-V requested one.
            let atomic = access.contains(StorageAccess::ATOMIC);
            let access = (access - StorageAccess::ATOMIC) | StorageAccess::LOAD;
            Ok((
                kind,
                if atomic {
                    access | StorageAccess::STORE
                } else {
                    access
                },
            ))
        }
        ImageClass::Sampled { multi: true, .. } => {
            Err(GpuError::Unsupported("multisampled texel buffers"))
        }
        ImageClass::Depth { .. } => Err(GpuError::Unsupported("depth texel buffers")),
    }
}

struct FunctionLowering<'a> {
    globals: &'a [Handle<naga::GlobalVariable>],
    map: Vec<Handle<Expression>>,
    spans: Vec<(Handle<Expression>, Handle<Expression>)>,
    texel: Vec<bool>,
}

impl<'a> FunctionLowering<'a> {
    fn new(globals: &'a [Handle<naga::GlobalVariable>]) -> Self {
        Self {
            globals,
            map: Vec::new(),
            spans: Vec::new(),
            texel: Vec::new(),
        }
    }

    fn lower(mut self, function: &mut naga::Function) -> Result<()> {
        let mut old = mem::take(&mut function.expressions);
        let mut expressions = Arena::new();
        for (old_handle, expression, span) in old.drain() {
            let first = expressions.len();
            let is_texel = self.is_texel(&expression);
            let mapped = self.expression(expression, span, is_texel, &mut expressions)?;
            self.map.push(mapped);
            self.texel.push(is_texel);
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
        self.block(&mut function.body, &mut expressions)?;
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

    fn is_texel(&self, expression: &Expression) -> bool {
        match expression {
            Expression::GlobalVariable(handle) => self.globals.contains(handle),
            Expression::Access { base, .. } | Expression::AccessIndex { base, .. } => {
                self.texel[base.index()]
            }
            _ => false,
        }
    }

    fn expression(
        &self,
        mut expression: Expression,
        span: Span,
        is_texel: bool,
        expressions: &mut Arena<Expression>,
    ) -> Result<Handle<Expression>> {
        let original = expression.clone();
        remap::expression(&self.map, &mut expression);
        match original {
            Expression::ImageLoad {
                image, coordinate, ..
            } if self.texel[image.index()] => {
                let field = expressions.append(
                    Expression::AccessIndex {
                        base: self.map[image.index()],
                        index: 0,
                    },
                    span,
                );
                let pointer = expressions.append(
                    Expression::Access {
                        base: field,
                        index: self.map[coordinate.index()],
                    },
                    span,
                );
                Ok(expressions.append(Expression::Load { pointer }, span))
            }
            Expression::ImageQuery { image, query } if self.texel[image.index()] => match query {
                naga::ImageQuery::Size { level: None } => {
                    let field = expressions.append(
                        Expression::AccessIndex {
                            base: self.map[image.index()],
                            index: 0,
                        },
                        span,
                    );
                    Ok(expressions.append(Expression::ArrayLength(field), span))
                }
                _ => Err(GpuError::Unsupported("invalid texel-buffer image query")),
            },
            Expression::ImageSample { image, .. } if self.texel[image.index()] => {
                Err(GpuError::Unsupported("sampling a texel buffer"))
            }
            _ => {
                let _ = is_texel;
                Ok(expressions.append(expression, span))
            }
        }
    }

    fn block(&self, block: &mut naga::Block, expressions: &mut Arena<Expression>) -> Result<()> {
        let mut rebuilt = naga::Block::with_capacity(block.len());
        for (mut statement, span) in mem::take(block).span_into_iter() {
            match &mut statement {
                Statement::ImageStore {
                    image,
                    coordinate,
                    array_index,
                    value,
                } if self.texel[image.index()] => {
                    if array_index.is_some() {
                        return Err(GpuError::Unsupported("arrayed texel-buffer store"));
                    }
                    let field = expressions.append(
                        Expression::AccessIndex {
                            base: self.map[image.index()],
                            index: 0,
                        },
                        span,
                    );
                    let pointer = expressions.append(
                        Expression::Access {
                            base: field,
                            index: self.map[coordinate.index()],
                        },
                        span,
                    );
                    rebuilt.push(
                        Statement::Emit(naga::Range::new_from_bounds(field, pointer)),
                        span,
                    );
                    rebuilt.push(
                        Statement::Store {
                            pointer,
                            value: self.map[value.index()],
                        },
                        span,
                    );
                }
                Statement::ImageAtomic {
                    image,
                    coordinate,
                    array_index,
                    fun,
                    value,
                    result,
                } if self.texel[image.index()] => {
                    if array_index.is_some() {
                        return Err(GpuError::Unsupported("arrayed texel-buffer atomic"));
                    }
                    let field = expressions.append(
                        Expression::AccessIndex {
                            base: self.map[image.index()],
                            index: 0,
                        },
                        span,
                    );
                    let pointer = expressions.append(
                        Expression::Access {
                            base: field,
                            index: self.map[coordinate.index()],
                        },
                        span,
                    );
                    rebuilt.push(
                        Statement::Emit(naga::Range::new_from_bounds(field, pointer)),
                        span,
                    );
                    let fun = match *fun {
                        naga::AtomicFunction::Exchange { compare } => {
                            naga::AtomicFunction::Exchange {
                                compare: compare.map(|handle| self.map[handle.index()]),
                            }
                        }
                        other => other,
                    };
                    rebuilt.push(
                        Statement::Atomic {
                            pointer,
                            fun,
                            value: self.map[value.index()],
                            result: result.map(|handle| self.map[handle.index()]),
                        },
                        span,
                    );
                }
                Statement::Block(nested) => {
                    self.block(nested, expressions)?;
                    rebuilt.push(statement, span);
                }
                Statement::If {
                    condition,
                    accept,
                    reject,
                } => {
                    *condition = self.map[condition.index()];
                    self.block(accept, expressions)?;
                    self.block(reject, expressions)?;
                    rebuilt.push(statement, span);
                }
                Statement::Switch { selector, cases } => {
                    *selector = self.map[selector.index()];
                    for case in cases {
                        self.block(&mut case.body, expressions)?;
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
                    self.block(body, expressions)?;
                    self.block(continuing, expressions)?;
                    rebuilt.push(statement, span);
                }
                _ => {
                    remap::statement(&self.map, &self.spans, &mut statement);
                    rebuilt.push(statement, span);
                }
            }
        }
        *block = rebuilt;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(class: naga::ImageClass) -> naga::Module {
        let mut module = naga::Module::default();
        let image = module.types.insert(
            Type {
                name: Some("source".into()),
                inner: TypeInner::Image {
                    dim: naga::ImageDimension::Buffer,
                    arrayed: false,
                    class,
                },
            },
            Span::default(),
        );
        module.global_variables.append(
            naga::GlobalVariable {
                name: Some("source".into()),
                space: naga::AddressSpace::Handle,
                binding: Some(naga::ResourceBinding {
                    group: 0,
                    binding: 0,
                }),
                ty: image,
                init: None,
            },
            Span::default(),
        );
        module
    }

    fn entry(module: &mut naga::Module) -> &mut naga::Function {
        module.entry_points.push(naga::EntryPoint {
            name: "main".into(),
            stage: naga::ShaderStage::Compute,
            early_depth_test: None,
            workgroup_size: [1, 1, 1],
            workgroup_size_overrides: None,
            function: naga::Function::default(),
        });
        &mut module.entry_points.last_mut().unwrap().function
    }

    fn wgsl(module: &mut naga::Module) -> String {
        TexelBuffers::lower(module, None).unwrap();
        super::super::ShaderModule::new(module).wgsl().unwrap()
    }

    #[test]
    fn uniform_uint_load_and_size_become_typed_samplerless_storage_reads() {
        let mut module = module(naga::ImageClass::Sampled {
            kind: naga::ScalarKind::Uint,
            multi: false,
        });
        let global = module.global_variables.iter().next().unwrap().0;
        let function = entry(&mut module);
        let image = function
            .expressions
            .append(Expression::GlobalVariable(global), Span::default());
        let coordinate = function
            .expressions
            .append(Expression::Literal(naga::Literal::U32(2)), Span::default());
        let load = function.expressions.append(
            Expression::ImageLoad {
                image,
                coordinate,
                array_index: None,
                sample: None,
                level: None,
            },
            Span::default(),
        );
        let size = function.expressions.append(
            Expression::ImageQuery {
                image,
                query: naga::ImageQuery::Size { level: None },
            },
            Span::default(),
        );
        function.body.push(
            Statement::Emit(naga::Range::new_from_bounds(coordinate, size)),
            Span::default(),
        );
        TexelBuffers::lower(&mut module, None).unwrap();
        assert!(module.entry_points[0]
            .function
            .expressions
            .iter()
            .any(|(_, expression)| matches!(expression, Expression::ArrayLength(_))));
        let output = super::super::ShaderModule::new(&mut module).wgsl().unwrap();
        assert!(output.contains("var<storage>"), "{output}");
        assert!(output.contains("array<vec4<u32>>"), "{output}");
        assert!(!output.contains("texture_buffer"), "{output}");
        let _ = load;
    }

    #[test]
    fn storage_float_write_becomes_typed_storage_array_write() {
        let mut module = module(naga::ImageClass::Storage {
            format: naga::StorageFormat::Rgba8Unorm,
            access: naga::StorageAccess::LOAD | naga::StorageAccess::STORE,
        });
        let global = module.global_variables.iter().next().unwrap().0;
        let vec4 = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Vector {
                    size: naga::VectorSize::Quad,
                    scalar: naga::Scalar::F32,
                },
            },
            Span::default(),
        );
        let function = entry(&mut module);
        let image = function
            .expressions
            .append(Expression::GlobalVariable(global), Span::default());
        let coordinate = function
            .expressions
            .append(Expression::Literal(naga::Literal::U32(1)), Span::default());
        let components = [0.25, 0.5, 0.75, 1.0]
            .map(|value| {
                function.expressions.append(
                    Expression::Literal(naga::Literal::F32(value)),
                    Span::default(),
                )
            })
            .to_vec();
        let value = function.expressions.append(
            Expression::Compose {
                ty: vec4,
                components,
            },
            Span::default(),
        );
        function.body.push(
            Statement::Emit(naga::Range::new_from_bounds(coordinate, value)),
            Span::default(),
        );
        function.body.push(
            Statement::ImageStore {
                image,
                coordinate,
                array_index: None,
                value,
            },
            Span::default(),
        );
        let output = wgsl(&mut module);
        assert!(output.contains("var<storage, read_write>"), "{output}");
        assert!(output.contains("array<vec4<f32>>"), "{output}");
        assert!(output.contains(".texels[1u] ="), "{output}");
    }
}
