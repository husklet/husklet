//! Native packed-buffer views and the specialization key used by Vulkan texel shaders.

use hl_gpu::protocol::model::descriptor::{BindGroupDesc, BindResource};
use hl_gpu::protocol::model::descriptor::{
    ComputePipelineDesc, PipelineLayout, RenderMultisample, RenderPipelineDesc,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::buffer::WgpuBuffer;
use crate::pipeline::PipelineNative;
use crate::WgpuExecutor;

const MAX_PIPELINE_VARIANTS: usize = 64;

pub(crate) struct PipelineCache<P> {
    variants: std::collections::HashMap<Vec<Specialization>, P>,
    order: std::collections::VecDeque<Vec<Specialization>>,
}

impl<P: Clone> PipelineCache<P> {
    fn new() -> Self {
        Self {
            variants: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
        }
    }

    pub(crate) fn get(&mut self, key: &[Specialization]) -> Option<P> {
        self.variants.get(key).cloned()
    }

    pub(crate) fn insert(&mut self, key: Vec<Specialization>, pipeline: P) {
        if self.variants.contains_key(&key) {
            self.variants.insert(key, pipeline);
            return;
        }
        if self.variants.len() == MAX_PIPELINE_VARIANTS {
            if let Some(oldest) = self.order.pop_front() {
                self.variants.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.variants.insert(key, pipeline);
    }
}

pub(crate) struct ComputeSpecializer {
    pub(crate) desc: ComputePipelineDesc,
    pub(crate) layout: PipelineLayout,
    pub(crate) words: Vec<u32>,
    pub(crate) variants: std::sync::Mutex<PipelineCache<wgpu::ComputePipeline>>,
}

pub(crate) struct RenderSpecializer {
    pub(crate) desc: RenderPipelineDesc,
    pub(crate) layout: PipelineLayout,
    pub(crate) multisample: RenderMultisample,
    pub(crate) vertex_words: Vec<u32>,
    pub(crate) fragment_words: Option<Vec<u32>>,
    pub(crate) variants: std::sync::Mutex<PipelineCache<wgpu::RenderPipeline>>,
}

impl RenderSpecializer {
    pub(crate) fn new(
        desc: RenderPipelineDesc,
        layout: PipelineLayout,
        multisample: RenderMultisample,
        vertex_words: Vec<u32>,
        fragment_words: Option<Vec<u32>>,
    ) -> Self {
        Self {
            desc,
            layout,
            multisample,
            vertex_words,
            fragment_words,
            variants: std::sync::Mutex::new(PipelineCache::new()),
        }
    }
}

impl ComputeSpecializer {
    pub(crate) fn new(
        desc: ComputePipelineDesc,
        layout: PipelineLayout,
        words: Vec<u32>,
    ) -> Self {
        Self {
            desc,
            layout,
            words,
            variants: std::sync::Mutex::new(PipelineCache::new()),
        }
    }
}

impl WgpuExecutor {
    pub(crate) fn compute_pipeline_for(
        &self,
        resources: &SessionResources,
        id: u32,
        specialization: &[Specialization],
    ) -> Result<wgpu::ComputePipeline> {
        let PipelineNative::Compute {
            pipeline, texel, ..
        } = PipelineNative::get(resources, id)?
        else {
            return Err(GpuError::Unsupported("wgpu: dispatch on a render pipeline"));
        };
        let Some(texel) = texel else {
            return Ok(pipeline.clone());
        };
        let mut variants = texel
            .variants
            .lock()
            .map_err(|_| GpuError::Invalid("wgpu: texel pipeline cache poisoned"))?;
        if let Some(pipeline) = variants.get(specialization) {
            return Ok(pipeline);
        }
        let pipeline = self.compile_compute_texel(texel, specialization)?;
        variants.insert(specialization.to_vec(), pipeline.clone());
        Ok(pipeline)
    }

    fn compile_compute_texel(
        &self,
        recipe: &ComputeSpecializer,
        specialization: &[Specialization],
    ) -> Result<wgpu::ComputePipeline> {
        let (source, reflected) = crate::wgsl::Spirv::translate_reflect_texel(
            &recipe.words,
            &recipe.layout,
            specialization,
        )?;
        let module = self.gpu.shader_module("hl-spirv-texel", source)?;
        let mut merged: std::collections::BTreeMap<_, _> = reflected
            .used_for(&recipe.desc.compute.entry)
            .iter()
            .map(|binding| {
                (
                    (binding.group, binding.binding),
                    (wgpu::ShaderStages::COMPUTE, binding.kind, binding.count),
                )
            })
            .collect();
        Self::apply_authoritative_counts(&mut merged, Some(&recipe.layout))?;
        let group_layouts = self.build_render_bind_group_layouts(&merged)?;
        let layout_refs = group_layouts.iter().collect::<Vec<_>>();
        let push_constant_ranges = self
            .gpu
            .device
            .features()
            .contains(wgpu::Features::PUSH_CONSTANTS)
            .then(|| wgpu::PushConstantRange {
                stages: wgpu::ShaderStages::COMPUTE,
                range: 0..self.gpu.device.limits().max_push_constant_size,
            })
            .into_iter()
            .collect::<Vec<_>>();
        let layout = self
            .gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-compute-texel-pl"),
                bind_group_layouts: &layout_refs,
                push_constant_ranges: &push_constant_ranges,
            });
        self.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = self
            .gpu
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("hl-compute-texel"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(recipe.desc.compute.entry.as_str()),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        if let Some(error) = pollster::block_on(self.gpu.device.pop_error_scope()) {
            return Err(GpuError::Kernel(format!(
                "wgpu: specialized texel compute pipeline failed: {error:?}"
            )));
        }
        Ok(pipeline)
    }
}

/// One shader-visible formatted view. The native binding aliases the original Vulkan buffer; there is no
/// expanded shadow, copy, or writeback. This is load-bearing for ordinary SSBO/texel aliases and for
/// visibility between dispatches and render passes.
#[derive(Clone)]
pub(crate) struct View {
    pub(crate) binding: u32,
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) offset: u64,
    pub(crate) size: u64,
}

/// The bounded part of a bound view that changes shader code: format/access plus native-alignment prefix
/// and final-word padding. Absolute offsets and ranges remain bind-group state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Specialization {
    pub(crate) group: u32,
    pub(crate) binding: u32,
    pub(crate) format: TextureFormat,
    pub(crate) writable: bool,
    pub(crate) prefix_words: u32,
    pub(crate) tail_padding: u8,
}

impl std::hash::Hash for Specialization {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(
            &(
                self.group,
                self.binding,
                self.format.to_u32(),
                self.writable,
                self.prefix_words,
                self.tail_padding,
            ),
            state,
        );
    }
}

impl Ord for Specialization {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (
            self.group,
            self.binding,
            self.format.to_u32(),
            self.writable,
            self.prefix_words,
            self.tail_padding,
        )
            .cmp(&(
                other.group,
                other.binding,
                other.format.to_u32(),
                other.writable,
                other.prefix_words,
                other.tail_padding,
            ))
    }
}

impl PartialOrd for Specialization {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn views(
    resources: &SessionResources,
    descriptor: &BindGroupDesc,
    alignment: u64,
) -> Result<Vec<View>> {
    if alignment == 0 || !alignment.is_multiple_of(4) {
        return Err(GpuError::Invalid("wgpu: zero storage-buffer offset alignment"));
    }
    descriptor
        .entries
        .iter()
        .filter_map(|entry| match entry.resource {
            BindResource::TexelBuffer {
                id, offset, size, ..
            } => Some((entry.binding, id, offset, size)),
            _ => None,
        })
        .map(|(binding, id, offset, size)| {
            let source = WgpuBuffer::get(resources, id)?;
            let end = offset.checked_add(size).ok_or(GpuError::OutOfBounds)?;
            if size == 0 || end > source.size || !offset.is_multiple_of(16) {
                return Err(GpuError::OutOfBounds);
            }
            let native_offset = offset / alignment * alignment;
            let prefix = offset - native_offset;
            let native_size = prefix
                .checked_add(size)
                .ok_or(GpuError::OutOfBounds)?
                .div_ceil(4) * 4;
            let native_end = native_offset.checked_add(native_size).ok_or(GpuError::OutOfBounds)?;
            if native_end > WgpuBuffer::allocation_size(source.size).max(4) {
                return Err(GpuError::OutOfBounds);
            }
            Ok(View {
                binding,
                buffer: source.buffer.clone(),
                offset: native_offset,
                size: native_size,
            })
        })
        .collect()
}

pub(crate) fn key<'a>(
    groups: impl IntoIterator<Item = &'a BindGroupDesc>,
    alignment: u64,
) -> Result<Vec<Specialization>> {
    if alignment == 0 || !alignment.is_multiple_of(4) {
        return Err(GpuError::Invalid("wgpu: zero storage-buffer offset alignment"));
    }
    let mut key = groups
        .into_iter()
        .flat_map(|descriptor| {
            descriptor.entries.iter().filter_map(move |entry| {
                let BindResource::TexelBuffer {
                    offset, size, format, writable, ..
                } = entry.resource
                else {
                    return None;
                };
                Some(Specialization {
                    group: descriptor.set,
                    binding: entry.binding,
                    format,
                    writable,
                    prefix_words: ((offset % alignment) / 4) as u32,
                    tail_padding: ((4 - ((offset % 4 + size % 4) % 4)) % 4) as u8,
                })
            })
        })
        .collect::<Vec<_>>();
    key.sort_unstable();
    key.dedup();
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specialization(prefix_words: u32) -> Vec<Specialization> {
        vec![Specialization {
            group: 0,
            binding: 0,
            format: TextureFormat::R8Unorm,
            writable: false,
            prefix_words,
            tail_padding: 0,
        }]
    }

    #[test]
    fn pipeline_variant_cache_evicts_instead_of_growing_permanently() {
        let mut cache = PipelineCache::new();
        for prefix in 0..=MAX_PIPELINE_VARIANTS as u32 {
            cache.insert(specialization(prefix), prefix);
        }
        assert_eq!(cache.variants.len(), MAX_PIPELINE_VARIANTS);
        assert!(cache.get(&specialization(0)).is_none());
        assert_eq!(cache.get(&specialization(MAX_PIPELINE_VARIANTS as u32)), Some(MAX_PIPELINE_VARIANTS as u32));
    }
}
