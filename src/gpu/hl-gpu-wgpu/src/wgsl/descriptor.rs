//! Fixed Vulkan buffer and storage-image arrays lowered to scalar host bindings.

use std::mem;

use hl_gpu::protocol::model::descriptor::{PipelineBindingKind, PipelineLayout};
use hl_gpu::{GpuError, Result};
use naga::{
    Arena, BinaryOperator, Expression, Function, GlobalVariable, Handle, Literal, LocalVariable,
    Span, Type, TypeInner,
};

mod image;
mod mutation;
mod pointer;
pub(super) mod remap;
mod value;

use remap::{block as remap_block, dedup_emits, expression as remap_expression};

type ScalarGlobals = (
    Handle<GlobalVariable>,
    Vec<Handle<GlobalVariable>>,
    Handle<Type>,
);

#[derive(Clone)]
enum Pointer {
    Array {
        globals: Vec<Handle<GlobalVariable>>,
        ty: Handle<Type>,
    },
    Selected {
        pointers: Vec<Handle<Expression>>,
        selector: Handle<Expression>,
        ty: Handle<Type>,
    },
}

/// Scalarize fixed buffer and storage-image descriptor arrays.
///
/// Metal's wgpu backend does not expose buffer binding arrays. Vulkan nevertheless requires dynamic
/// indexing for Dawn's baseline. Each descriptor becomes a normal host binding. Dynamic reads and
/// queries become bounded selects; mutations become switches with no-op defaults. This preserves
/// separate resources without requiring Metal resource arrays.
pub(super) struct ScalarArrays;

impl ScalarArrays {
    pub(super) fn lower(module: &mut naga::Module, layout: &PipelineLayout) -> Result<()> {
        let arrays = layout
            .bindings
            .iter()
            .filter(|binding| {
                matches!(
                    binding.kind,
                    PipelineBindingKind::UniformBuffer
                        | PipelineBindingKind::StorageBuffer
                        | PipelineBindingKind::StorageTexture
                        | PipelineBindingKind::UniformTexelBuffer
                        | PipelineBindingKind::StorageTexelBuffer
                ) && binding.count > 1
            })
            .copied()
            .collect::<Vec<_>>();
        if arrays.is_empty() {
            return Ok(());
        }

        let mut globals = Vec::new();
        for binding in arrays {
            let (handle, base) = module
                .global_variables
                .iter()
                .find_map(|(handle, variable)| {
                    (variable.binding.as_ref().is_some_and(|resource| {
                        resource.group == binding.group && resource.binding == binding.binding
                    }))
                    .then(|| match module.types[variable.ty].inner {
                        TypeInner::BindingArray { base, .. } => Some((handle, base)),
                        _ => None,
                    })
                    .flatten()
                })
                .ok_or(GpuError::Invalid(
                    "buffer descriptor array is absent from SPIR-V",
                ))?;
            let original = module.global_variables[handle].clone();
            module.global_variables[handle].ty = base;
            let mut elements = vec![handle];
            for element in 1..binding.count {
                let mut variable = original.clone();
                variable.name = original
                    .name
                    .as_ref()
                    .map(|name| format!("{name}_{element}"));
                variable.ty = base;
                variable.binding.as_mut().unwrap().binding =
                    layout.scalar_binding(binding.group, binding.binding, element)?;
                elements.push(module.global_variables.append(variable, Span::default()));
            }
            globals.push((handle, elements, base));
        }

        let component_types = module
            .types
            .iter()
            .filter_map(|(_, ty)| match ty.inner {
                TypeInner::Vector { scalar, .. } => Some(TypeInner::Scalar(scalar)),
                TypeInner::Matrix { rows, scalar, .. } => {
                    Some(TypeInner::Vector { size: rows, scalar })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for inner in component_types {
            module
                .types
                .insert(Type { name: None, inner }, Span::default());
        }
        for scalar in [naga::Scalar::U32, naga::Scalar::I32, naga::Scalar::F32] {
            module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Scalar(scalar),
                },
                Span::default(),
            );
            for size in [
                naga::VectorSize::Bi,
                naga::VectorSize::Tri,
                naga::VectorSize::Quad,
            ] {
                module.types.insert(
                    Type {
                        name: None,
                        inner: TypeInner::Vector { size, scalar },
                    },
                    Span::default(),
                );
            }
        }
        for (_, function) in module.functions.iter_mut() {
            FunctionLowering::new(&module.types, &globals).lower(function)?;
        }
        for entry in &mut module.entry_points {
            FunctionLowering::new(&module.types, &globals).lower(&mut entry.function)?;
        }
        Ok(())
    }
}

struct FunctionLowering<'a> {
    types: &'a naga::UniqueArena<Type>,
    globals: &'a [ScalarGlobals],
    map: Vec<Handle<Expression>>,
    spans: Vec<(Handle<Expression>, Handle<Expression>)>,
    pointers: Vec<Option<Pointer>>,
    atomic_results: Vec<Option<AtomicResult>>,
}

#[derive(Clone, Copy)]
struct AtomicResult {
    local: Handle<Expression>,
    ty: Handle<Type>,
    comparison: bool,
}

impl<'a> FunctionLowering<'a> {
    fn new(types: &'a naga::UniqueArena<Type>, globals: &'a [ScalarGlobals]) -> Self {
        Self {
            types,
            globals,
            map: Vec::new(),
            spans: Vec::new(),
            pointers: Vec::new(),
            atomic_results: Vec::new(),
        }
    }

    fn lower(mut self, function: &mut Function) -> Result<()> {
        let original_locals = function.local_variables.len();
        let mut old = mem::take(&mut function.expressions);
        let mut expressions = Arena::new();
        for (old_handle, expression, span) in old.drain() {
            let first = expressions.len();
            let (mapped, pointer, atomic) = self.expression(
                expression,
                span,
                &mut expressions,
                &mut function.local_variables,
            )?;
            self.map.push(mapped);
            self.pointers.push(pointer);
            self.atomic_results.push(atomic);
            let last = expressions.len();
            self.spans.push(if last > first {
                (
                    expressions.iter().nth(first).unwrap().0,
                    expressions.iter().nth(last.saturating_sub(1)).unwrap().0,
                )
            } else {
                (mapped, mapped)
            });
            debug_assert_eq!(old_handle.index() + 1, self.map.len());
        }
        self.block(&mut function.body, &mut expressions)?;
        dedup_emits(&mut function.body, &expressions);
        for (handle, local) in function.local_variables.iter_mut() {
            if handle.index() >= original_locals {
                continue;
            }
            if let Some(init) = local.init.as_mut() {
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

    fn expression(
        &self,
        mut expression: Expression,
        span: Span,
        expressions: &mut Arena<Expression>,
        locals: &mut Arena<LocalVariable>,
    ) -> Result<(Handle<Expression>, Option<Pointer>, Option<AtomicResult>)> {
        let original = expression.clone();
        remap_expression(&self.map, &mut expression);
        match original {
            Expression::GlobalVariable(global) => {
                if let Some((_, elements, ty)) = self
                    .globals
                    .iter()
                    .find(|(candidate, _, _)| *candidate == global)
                {
                    let handles: Vec<Handle<Expression>> = elements
                        .iter()
                        .map(|global| expressions.append(Expression::GlobalVariable(*global), span))
                        .collect();
                    let mapped = handles[0];
                    return Ok((
                        mapped,
                        Some(Pointer::Array {
                            globals: elements.clone(),
                            ty: *ty,
                        }),
                        None,
                    ));
                }
            }
            Expression::Access { base, index } => {
                if let Some(pointer) = self.pointers[base.index()].as_ref() {
                    let (mapped, pointer) = self.access(
                        pointer,
                        Some(self.map[index.index()]),
                        None,
                        span,
                        expressions,
                    )?;
                    return Ok((mapped, pointer, None));
                }
            }
            Expression::AccessIndex { base, index } => {
                if let Some(pointer) = self.pointers[base.index()].as_ref() {
                    let (mapped, pointer) =
                        self.access(pointer, None, Some(index), span, expressions)?;
                    return Ok((mapped, pointer, None));
                }
            }
            Expression::Load { pointer } => {
                if let Some(Pointer::Selected {
                    pointers,
                    selector,
                    ty,
                }) = self.pointers[pointer.index()].as_ref()
                {
                    return Ok((
                        self.selected_value(pointers, *selector, *ty, span, expressions)?,
                        None,
                        None,
                    ));
                }
            }
            Expression::ImageLoad {
                image,
                coordinate,
                array_index,
                sample,
                level,
            } => {
                if let Some(Pointer::Selected {
                    pointers,
                    selector,
                    ty,
                }) = self.pointers[image.index()].as_ref()
                {
                    return Ok((
                        self.image_load(
                            image::Load {
                                pointers: pointers.clone(),
                                selector: *selector,
                                coordinate: self.map[coordinate.index()],
                                array_index: array_index.map(|value| self.map[value.index()]),
                                sample: sample.map(|value| self.map[value.index()]),
                                level: level.map(|value| self.map[value.index()]),
                                image_ty: *ty,
                            },
                            span,
                            expressions,
                        )?,
                        None,
                        None,
                    ));
                }
            }
            Expression::ImageQuery { image, ref query } => {
                if let Some(Pointer::Selected {
                    pointers,
                    selector,
                    ty,
                }) = self.pointers[image.index()].as_ref()
                {
                    let mut query = *query;
                    if let naga::ImageQuery::Size { level: Some(level) } = &mut query {
                        *level = self.map[level.index()];
                    }
                    return Ok((
                        self.image_query(pointers, *selector, &query, *ty, span, expressions)?,
                        None,
                        None,
                    ));
                }
            }
            Expression::ArrayLength(pointer)
                if matches!(
                    self.pointers[pointer.index()],
                    Some(Pointer::Selected { .. })
                ) =>
            {
                return Err(GpuError::Unsupported(
                    "dynamic descriptor array length queries are unsupported",
                ));
            }
            Expression::AtomicResult { ty, comparison } => {
                let zero = expressions.append(Expression::ZeroValue(ty), span);
                let local = locals.append(
                    LocalVariable {
                        name: Some("_hl_descriptor_atomic".into()),
                        ty,
                        init: Some(zero),
                    },
                    span,
                );
                let pointer = expressions.append(Expression::LocalVariable(local), span);
                let value = expressions.append(Expression::Load { pointer }, span);
                return Ok((
                    value,
                    None,
                    Some(AtomicResult {
                        local: pointer,
                        ty,
                        comparison,
                    }),
                ));
            }
            _ => {}
        }
        Ok((expressions.append(expression, span), None, None))
    }
}

#[cfg(test)]
mod tests;
