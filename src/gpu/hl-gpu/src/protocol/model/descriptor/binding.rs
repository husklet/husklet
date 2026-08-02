//! Pipeline layout + bind-group descriptors: the authoritative binding cardinality the API layer
//! supplies, and the bound resources a bind group carries.
//!
//! Split out of [`super`] so the layout/binding vocabulary lives in one place; the parent re-exports
//! these types, so `descriptor::*` is unchanged for callers.

use super::super::{
    enums::TextureFormat,
    error::{GpuError, Result},
};

/// Authoritative descriptor cardinality from the API pipeline layout. Shader reflection still supplies
/// binding kind and stage visibility; it cannot reliably recover Vulkan descriptor-array counts from
/// SPIR-V because descriptor arrays and fixed-size buffer payload arrays share the same Naga shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PipelineBinding {
    pub group: u32,
    pub binding: u32,
    pub count: u32,
    pub kind: PipelineBindingKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PipelineBindingKind {
    UniformBuffer,
    StorageBuffer,
    SampledTexture,
    StorageTexture,
    Sampler,
    CombinedImageSampler,
    /// SPIR-V `DimBuffer`, lowered to a read-only typed storage buffer before WGSL emission.
    UniformTexelBuffer,
    /// SPIR-V storage `DimBuffer`, lowered to a read-write typed storage buffer.
    StorageTexelBuffer,
}

impl PipelineBindingKind {
    pub fn to_u32(self) -> u32 {
        self as u32
    }

    pub fn from_u32(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::UniformBuffer),
            1 => Ok(Self::StorageBuffer),
            2 => Ok(Self::SampledTexture),
            3 => Ok(Self::StorageTexture),
            4 => Ok(Self::Sampler),
            5 => Ok(Self::CombinedImageSampler),
            6 => Ok(Self::UniformTexelBuffer),
            7 => Ok(Self::StorageTexelBuffer),
            _ => Err(GpuError::BadTag(value)),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PipelineLayout {
    pub bindings: Vec<PipelineBinding>,
}

impl PipelineLayout {
    /// Native scalar binding for one element of a fixed-size descriptor array.
    ///
    /// Element zero retains the guest binding. Remaining elements occupy a deterministic,
    /// collision-free tail after the group's greatest guest binding. The mapping depends on the
    /// complete authoritative layout, so every shader stage and descriptor-set encoder derives the
    /// same slots without extending the wire protocol.
    pub fn scalar_binding(&self, group: u32, binding: u32, element: u32) -> Result<u32> {
        let declared = self
            .bindings
            .iter()
            .find(|item| item.group == group && item.binding == binding)
            .ok_or(GpuError::Invalid(
                "descriptor binding is absent from pipeline layout",
            ))?;
        if element >= declared.count {
            return Err(GpuError::OutOfBounds);
        }
        if element == 0 {
            return Ok(binding);
        }
        let tail = self
            .bindings
            .iter()
            .filter(|item| item.group == group)
            .map(|item| item.binding)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(GpuError::OutOfBounds)?;
        let preceding = self
            .bindings
            .iter()
            .filter(|item| item.group == group && item.binding < binding && item.count > 1)
            .try_fold(0u32, |total, item| {
                total
                    .checked_add(item.count - 1)
                    .ok_or(GpuError::OutOfBounds)
            })?;
        tail.checked_add(preceding)
            .and_then(|slot| slot.checked_add(element - 1))
            .ok_or(GpuError::OutOfBounds)
    }
}

/// A single binding within a bind group.
#[derive(Clone, PartialEq, Debug)]
pub enum BindResource {
    Buffer {
        id: u32,
        offset: u64,
        size: u64,
    },
    Texture {
        id: u32,
    },
    Sampler {
        id: u32,
    },
    BufferArray {
        elements: Vec<BufferBinding>,
    },
    TextureArray {
        ids: Vec<u32>,
    },
    SamplerArray {
        ids: Vec<u32>,
    },
    /// A Vulkan buffer view. The executor binds the original packed buffer and specializes the shader's
    /// raw-buffer loads and stores for this format, preserving aliases without a shadow or writeback.
    TexelBuffer {
        id: u32,
        offset: u64,
        size: u64,
        format: TextureFormat,
        writable: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferBinding {
    pub id: u32,
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, PartialEq, Debug)]
pub struct BindEntry {
    pub binding: u32,
    pub resource: BindResource,
}

#[derive(Clone, PartialEq, Debug)]
pub struct BindGroupDesc {
    /// Which pipeline layout set index this group binds to.
    pub set: u32,
    pub entries: Vec<BindEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_arrays_scalarize_after_guest_bindings_without_collisions() {
        let layout = PipelineLayout {
            bindings: vec![
                PipelineBinding {
                    group: 0,
                    binding: 1,
                    count: 3,
                    kind: PipelineBindingKind::UniformBuffer,
                },
                PipelineBinding {
                    group: 0,
                    binding: 4,
                    count: 1,
                    kind: PipelineBindingKind::StorageBuffer,
                },
                PipelineBinding {
                    group: 0,
                    binding: 7,
                    count: 2,
                    kind: PipelineBindingKind::SampledTexture,
                },
                PipelineBinding {
                    group: 1,
                    binding: 1,
                    count: 2,
                    kind: PipelineBindingKind::Sampler,
                },
            ],
        };

        assert_eq!(layout.scalar_binding(0, 1, 0).unwrap(), 1);
        assert_eq!(layout.scalar_binding(0, 1, 1).unwrap(), 8);
        assert_eq!(layout.scalar_binding(0, 1, 2).unwrap(), 9);
        assert_eq!(layout.scalar_binding(0, 7, 0).unwrap(), 7);
        assert_eq!(layout.scalar_binding(0, 7, 1).unwrap(), 10);
        assert_eq!(layout.scalar_binding(1, 1, 1).unwrap(), 2);
        assert!(matches!(
            layout.scalar_binding(0, 7, 2),
            Err(GpuError::OutOfBounds)
        ));
    }
}
