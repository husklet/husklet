use std::collections::HashMap;
use std::mem;

use hl_gpu::{GpuError, Result};
use naga::{Arena, Expression, Handle, Span, Statement, Type, TypeInner};

use crate::texel_buffer::Specialization;

use super::super::descriptor::remap;
mod format;
mod memory;
mod store;

use format::{decode, encode, format_shape};
use memory::{raw_pointer, raw_pointer_in, raw_words};
use store::build_partial_store;

#[derive(Clone, Copy)]
struct Helpers {
    load: Handle<naga::Function>,
    store: Option<Handle<naga::Function>>,
    bytes: u32,
    prefix_bytes: u32,
    tail_padding: u32,
}

pub(super) fn lower(module: &mut naga::Module, specialization: &[Specialization]) -> Result<()> {
    let mut helpers = HashMap::new();
    let globals = module
        .global_variables
        .iter()
        .map(|(handle, variable)| {
            let TypeInner::Image {
                dim: naga::ImageDimension::Buffer,
                arrayed: false,
                ..
            } = module.types[variable.ty].inner
            else {
                return Ok(None);
            };
            let binding = variable.binding.as_ref().ok_or(GpuError::Invalid(
                "wgpu: texel-buffer shader global has no resource binding",
            ))?;
            let spec = specialization.iter().find(|spec| {
                spec.group == binding.group && spec.binding == binding.binding
            }).ok_or(GpuError::Invalid(
                "wgpu: texel-buffer shader global has no bound specialization",
            ))?;
            Ok(Some((handle, *spec)))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if globals.is_empty() {
        return Ok(());
    }
    for (global, spec) in globals {
        let helper = build_helpers(module, global, spec)?;
        let atomic_kind = if spec.format == hl_gpu::protocol::model::enums::TextureFormat::R32Sint {
            naga::ScalarKind::Sint
        } else {
            naga::ScalarKind::Uint
        };
        let word = if spec.writable {
            module.types.insert(
                Type {
                    name: None,
                    inner: TypeInner::Atomic(naga::Scalar { kind: atomic_kind, width: 4 }),
                },
                Span::default(),
            )
        } else {
            scalar(module, naga::ScalarKind::Uint)
        };
        let array = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Array {
                    base: word,
                    size: naga::ArraySize::Dynamic,
                    stride: 4,
                },
            },
            Span::default(),
        );
        let structure = module.types.insert(
            Type {
                name: Some(format!("_hl_packed_texel_buffer_{}_{}", spec.group, spec.binding)),
                inner: TypeInner::Struct {
                    members: vec![naga::StructMember {
                        name: Some("words".into()),
                        ty: array,
                        binding: None,
                        offset: 0,
                    }],
                    span: 4,
                },
            },
            Span::default(),
        );
        let variable = &mut module.global_variables[global];
        variable.ty = structure;
        variable.space = naga::AddressSpace::Storage {
            access: if spec.writable {
                naga::StorageAccess::LOAD | naga::StorageAccess::STORE
            } else {
                naga::StorageAccess::LOAD
            },
        };
        helpers.insert(global, helper);
    }
    reorder_helpers(module, &mut helpers);
    for (_, function) in module.functions.iter_mut() {
        FunctionLowering::new(&helpers).lower(function)?;
    }
    for entry in &mut module.entry_points {
        FunctionLowering::new(&helpers).lower(&mut entry.function)?;
    }
    Ok(())
}

fn reorder_helpers(
    module: &mut naga::Module,
    helpers: &mut HashMap<Handle<naga::GlobalVariable>, Helpers>,
) {
    let helper_handles = helpers
        .values()
        .flat_map(|helper| [Some(helper.load), helper.store])
        .flatten()
        .collect::<std::collections::HashSet<_>>();
    let mut old = mem::take(&mut module.functions);
    let mut entries = old.drain().collect::<Vec<_>>();
    entries.sort_by_key(|(handle, _, _)| !helper_handles.contains(handle));
    let mut map = HashMap::new();
    for (old, function, span) in entries {
        let new = module.functions.append(function, span);
        map.insert(old, new);
    }
    for helper in helpers.values_mut() {
        helper.load = map[&helper.load];
        helper.store = helper.store.map(|handle| map[&handle]);
    }
    for (_, function) in module.functions.iter_mut() {
        remap_function_calls(function, &map);
    }
    for entry in &mut module.entry_points {
        remap_function_calls(&mut entry.function, &map);
    }
}

fn remap_function_calls(
    function: &mut naga::Function,
    map: &HashMap<Handle<naga::Function>, Handle<naga::Function>>,
) {
    for (_, expression) in function.expressions.iter_mut() {
        if let Expression::CallResult(handle) = expression {
            *handle = map[handle];
        }
    }
    fn block(
        body: &mut naga::Block,
        map: &HashMap<Handle<naga::Function>, Handle<naga::Function>>,
    ) {
        for statement in body.iter_mut() {
            match statement {
                Statement::Call { function, .. } => *function = map[function],
                Statement::Block(nested) => block(nested, map),
                Statement::If { accept, reject, .. } => {
                    block(accept, map);
                    block(reject, map);
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        block(&mut case.body, map);
                    }
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    block(body, map);
                    block(continuing, map);
                }
                _ => {}
            }
        }
    }
    block(&mut function.body, map);
}

fn scalar(module: &mut naga::Module, kind: naga::ScalarKind) -> Handle<Type> {
    module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Scalar(naga::Scalar { kind, width: 4 }),
        },
        Span::default(),
    )
}

fn vector(module: &mut naga::Module, kind: naga::ScalarKind, size: naga::VectorSize) -> Handle<Type> {
    module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Vector {
                size,
                scalar: naga::Scalar { kind, width: 4 },
            },
        },
        Span::default(),
    )
}

fn build_helpers(
    module: &mut naga::Module,
    global: Handle<naga::GlobalVariable>,
    spec: Specialization,
) -> Result<Helpers> {
    let (kind, bytes) = format_shape(spec.format)?;
    let words = bytes.div_ceil(4);
    let vec4 = vector(module, kind, naga::VectorSize::Quad);
    let index_ty = scalar(module, naga::ScalarKind::Sint);
    let mut load = naga::Function {
        name: Some(format!("_hl_texel_load_{}_{}", spec.group, spec.binding)),
        arguments: vec![naga::FunctionArgument {
            name: Some("index".into()),
            ty: index_ty,
            binding: None,
        }],
        result: Some(naga::FunctionResult {
            ty: vec4,
            binding: None,
        }),
        ..Default::default()
    };
    let index = load
        .expressions
        .append(Expression::FunctionArgument(0), Span::default());
    let atomic_kind = if spec.format == hl_gpu::protocol::model::enums::TextureFormat::R32Sint {
        naga::ScalarKind::Sint
    } else {
        naga::ScalarKind::Uint
    };
    let values = raw_words(&mut load, global, index, bytes, words, spec.prefix_words, atomic_kind);
    let decoded = decode(&mut load, spec.format, vec4, index, &values)?;
    load.body.push(
        Statement::Emit(naga::Range::new_from_bounds(index, decoded)),
        Span::default(),
    );
    load.body.push(
        Statement::Return {
            value: Some(decoded),
        },
        Span::default(),
    );
    let load = module.functions.append(load, Span::default());

    let store = if spec.writable {
        let mut store = naga::Function {
            name: Some(format!("_hl_texel_store_{}_{}", spec.group, spec.binding)),
            arguments: vec![
                naga::FunctionArgument {
                    name: Some("index".into()),
                    ty: index_ty,
                    binding: None,
                },
                naga::FunctionArgument {
                    name: Some("value".into()),
                    ty: vec4,
                    binding: None,
                },
            ],
            ..Default::default()
        };
        let index = store
            .expressions
            .append(Expression::FunctionArgument(0), Span::default());
        let value = store
            .expressions
            .append(Expression::FunctionArgument(1), Span::default());
        if bytes < 4 {
            build_partial_store(
                module,
                &mut store,
                global,
                spec.format,
                index,
                value,
                bytes,
                spec.prefix_words,
            )?;
            return Ok(Helpers {
                load,
                store: Some(module.functions.append(store, Span::default())),
                bytes,
                prefix_bytes: spec.prefix_words * 4,
                tail_padding: u32::from(spec.tail_padding),
            });
        }
        let current = raw_words(&mut store, global, index, bytes, words, spec.prefix_words, atomic_kind);
        let packed = encode(&mut store, spec.format, index, value, &current)?;
        for (word, packed) in packed.into_iter().enumerate() {
            let pointer = raw_pointer(&mut store, global, index, bytes, word as u32, spec.prefix_words);
            let packed = if atomic_kind == naga::ScalarKind::Sint {
                store.expressions.append(Expression::As {
                    expr: packed,
                    kind: naga::ScalarKind::Sint,
                    convert: None,
                }, Span::default())
            } else {
                packed
            };
            store.body.push(
                Statement::Emit(naga::Range::new_from_bounds(index, packed)),
                Span::default(),
            );
            store.body.push(Statement::Store { pointer, value: packed }, Span::default());
        }
        Some(module.functions.append(store, Span::default()))
    } else {
        None
    };
    Ok(Helpers {
        load,
        store,
        bytes,
        prefix_bytes: spec.prefix_words * 4,
        tail_padding: u32::from(spec.tail_padding),
    })
}

struct FunctionLowering<'a> {
    helpers: &'a HashMap<Handle<naga::GlobalVariable>, Helpers>,
    map: Vec<Handle<Expression>>,
    spans: Vec<(Handle<Expression>, Handle<Expression>)>,
    texel: Vec<Option<Handle<naga::GlobalVariable>>>,
    calls: Vec<(Handle<Expression>, Handle<naga::Function>, Vec<Handle<Expression>>)>,
}

impl<'a> FunctionLowering<'a> {
    fn new(helpers: &'a HashMap<Handle<naga::GlobalVariable>, Helpers>) -> Self {
        Self { helpers, map: Vec::new(), spans: Vec::new(), texel: Vec::new(), calls: Vec::new() }
    }

    fn lower(mut self, function: &mut naga::Function) -> Result<()> {
        let mut old = mem::take(&mut function.expressions);
        let mut expressions = Arena::new();
        for (old_handle, expression, span) in old.drain() {
            let first = expressions.len();
            let global = self.texel_global(&expression);
            let mapped = self.expression(old_handle, expression, span, global, &mut expressions)?;
            self.map.push(mapped);
            self.texel.push(global);
            let last = expressions.len();
            self.spans.push(if last > first {
                (expressions.iter().nth(first).unwrap().0, expressions.iter().nth(last - 1).unwrap().0)
            } else { (mapped, mapped) });
        }
        self.block(&mut function.body, &mut expressions)?;
        remap::dedup_emits(&mut function.body, &expressions);
        for (_, local) in function.local_variables.iter_mut() {
            if let Some(init) = &mut local.init { *init = self.map[init.index()]; }
        }
        function.named_expressions = mem::take(&mut function.named_expressions)
            .into_iter().map(|(handle, name)| (self.map[handle.index()], name)).collect();
        function.expressions = expressions;
        Ok(())
    }

    fn texel_global(&self, expression: &Expression) -> Option<Handle<naga::GlobalVariable>> {
        match expression {
            Expression::GlobalVariable(handle) if self.helpers.contains_key(handle) => Some(*handle),
            Expression::Access { base, .. } | Expression::AccessIndex { base, .. } => self.texel[base.index()],
            _ => None,
        }
    }

    fn expression(
        &mut self,
        old: Handle<Expression>,
        mut expression: Expression,
        span: Span,
        _global: Option<Handle<naga::GlobalVariable>>,
        expressions: &mut Arena<Expression>,
    ) -> Result<Handle<Expression>> {
        let original = expression.clone();
        remap::expression(&self.map, &mut expression);
        match original {
            Expression::ImageLoad { image, coordinate, .. }
                if self.texel[image.index()].is_some() => {
                let helper = self.helpers[&self.texel[image.index()].unwrap()].load;
                let result = expressions.append(Expression::CallResult(helper), span);
                self.calls.push((old, helper, vec![self.map[coordinate.index()]]));
                Ok(result)
            }
            Expression::ImageQuery { image, query: naga::ImageQuery::Size { level: None } }
                if self.texel[image.index()].is_some() => {
                let global = self.texel[image.index()].unwrap();
                let source = expressions.append(Expression::GlobalVariable(global), span);
                let field = expressions.append(Expression::AccessIndex { base: source, index: 0 }, span);
                let words = expressions.append(Expression::ArrayLength(field), span);
                let four = expressions.append(Expression::Literal(naga::Literal::U32(4)), span);
                let total_bytes = expressions.append(Expression::Binary { op: naga::BinaryOperator::Multiply, left: words, right: four }, span);
                let helper = self.helpers[&global];
                let excluded = expressions.append(Expression::Literal(naga::Literal::U32(helper.prefix_bytes + helper.tail_padding)), span);
                let logical_bytes = expressions.append(Expression::Binary { op: naga::BinaryOperator::Subtract, left: total_bytes, right: excluded }, span);
                let divisor = expressions.append(Expression::Literal(naga::Literal::U32(helper.bytes)), span);
                Ok(expressions.append(Expression::Binary { op: naga::BinaryOperator::Divide, left: logical_bytes, right: divisor }, span))
            }
            Expression::ImageSample { image, .. } if self.texel[image.index()].is_some() => Err(GpuError::Unsupported("sampling a texel buffer")),
            _ => Ok(expressions.append(expression, span)),
        }
    }

    fn block(&self, block: &mut naga::Block, expressions: &mut Arena<Expression>) -> Result<()> {
        let mut rebuilt = naga::Block::with_capacity(block.len());
        for (mut statement, span) in mem::take(block).span_into_iter() {
            match &mut statement {
                Statement::Emit(_) => {
                    remap::statement(&self.map, &self.spans, &mut statement);
                    let Statement::Emit(range) = statement else { unreachable!() };
                    let results = self
                        .calls
                        .iter()
                        .map(|(old, function, args)| {
                            (self.map[old.index()], (*function, args.clone()))
                        })
                        .collect::<HashMap<_, _>>();
                    let mut run: Option<(Handle<Expression>, Handle<Expression>)> = None;
                    for handle in range {
                        if let Some((function, args)) = results.get(&handle) {
                            if let Some((first, last)) = run.take() {
                                rebuilt.push(Statement::Emit(naga::Range::new_from_bounds(first, last)), span);
                            }
                            rebuilt.push(
                                Statement::Call {
                                    function: *function,
                                    arguments: args.clone(),
                                    result: Some(handle),
                                },
                                span,
                            );
                        } else if let Some((first, _)) = run {
                            run = Some((first, handle));
                        } else {
                            run = Some((handle, handle));
                        }
                    }
                    if let Some((first, last)) = run {
                        rebuilt.push(Statement::Emit(naga::Range::new_from_bounds(first, last)), span);
                    }
                }
                Statement::ImageStore { image, coordinate, array_index, value } if self.texel[image.index()].is_some() => {
                    if array_index.is_some() { return Err(GpuError::Unsupported("arrayed texel-buffer store")); }
                    let global = self.texel[image.index()].unwrap();
                    let helper = self.helpers[&global].store.ok_or(GpuError::Invalid("write through read-only texel buffer"))?;
                    rebuilt.push(Statement::Call { function: helper, arguments: vec![self.map[coordinate.index()], self.map[value.index()]], result: None }, span);
                }
                Statement::ImageAtomic { image, coordinate, array_index, fun, value, result }
                    if self.texel[image.index()].is_some() =>
                {
                    if array_index.is_some() {
                        return Err(GpuError::Unsupported("arrayed texel-buffer atomic"));
                    }
                    let global = self.texel[image.index()].unwrap();
                    let helper = self.helpers[&global];
                    if helper.bytes != 4 {
                        return Err(GpuError::Unsupported("atomic operation on a packed texel-buffer format"));
                    }
                    let first = expressions.len();
                    let pointer = raw_pointer_in(
                        expressions,
                        global,
                        self.map[coordinate.index()],
                        helper.bytes,
                        0,
                        helper.prefix_bytes / 4,
                    );
                    if expressions.len() > first {
                        let first = expressions.iter().nth(first).unwrap().0;
                        rebuilt.push(Statement::Emit(naga::Range::new_from_bounds(first, pointer)), span);
                    }
                    let fun = match *fun {
                        naga::AtomicFunction::Exchange { compare } => naga::AtomicFunction::Exchange {
                            compare: compare.map(|handle| self.map[handle.index()]),
                        },
                        other => other,
                    };
                    rebuilt.push(Statement::Atomic {
                        pointer,
                        fun,
                        value: self.map[value.index()],
                        result: result.map(|handle| self.map[handle.index()]),
                    }, span);
                }
                Statement::Block(nested) => { self.block(nested, expressions)?; rebuilt.push(statement, span); }
                Statement::If { condition, accept, reject } => {
                    *condition = self.map[condition.index()]; self.block(accept, expressions)?; self.block(reject, expressions)?; rebuilt.push(statement, span);
                }
                Statement::Switch { selector, cases } => {
                    *selector = self.map[selector.index()]; for case in cases { self.block(&mut case.body, expressions)?; } rebuilt.push(statement, span);
                }
                Statement::Loop { body, continuing, break_if } => {
                    self.block(body, expressions)?; self.block(continuing, expressions)?; if let Some(value) = break_if { *value = self.map[value.index()]; } rebuilt.push(statement, span);
                }
                _ => { remap::statement(&self.map, &self.spans, &mut statement); rebuilt.push(statement, span); }
            }
        }
        *block = rebuilt;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dim_buffer_global_without_a_matching_bound_specialization_is_refused() {
        let mut module = naga::Module::default();
        let image = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Image {
                    dim: naga::ImageDimension::Buffer,
                    arrayed: false,
                    class: naga::ImageClass::Storage {
                        format: naga::StorageFormat::Rgba8Unorm,
                        access: naga::StorageAccess::LOAD,
                    },
                },
            },
            Span::default(),
        );
        module.global_variables.append(
            naga::GlobalVariable {
                name: None,
                space: naga::AddressSpace::Handle,
                binding: Some(naga::ResourceBinding { group: 0, binding: 3 }),
                ty: image,
                init: None,
            },
            Span::default(),
        );
        assert!(matches!(lower(&mut module, &[]), Err(GpuError::Invalid(_))));
    }
}
