//! Compatibility audit for the Vulkan baseline consumed by Chrome 150's Dawn revision.
//!
//! This does not advertise or enable anything. It evaluates the feature and limit values already
//! reported by the ICD so a missing host guarantee has one exact diagnostic instead of making Dawn
//! silently discard the physical device.

use crate::types::{VkPhysicalDeviceFeatures, VkPhysicalDeviceLimits, VK_FALSE};

const SAMPLE_COUNT_1_AND_4: u32 = 0x1 | 0x4;

pub(super) struct DawnBaseline<'a> {
    features: &'a VkPhysicalDeviceFeatures,
    limits: &'a VkPhysicalDeviceLimits,
    max_buffer_size: u64,
}

impl<'a> DawnBaseline<'a> {
    pub(super) fn new(
        features: &'a VkPhysicalDeviceFeatures,
        limits: &'a VkPhysicalDeviceLimits,
        max_buffer_size: u64,
    ) -> Self {
        Self {
            features,
            limits,
            max_buffer_size,
        }
    }

    /// Missing requirements from Dawn revision `d089fc91e7e4881362463faf8efe9ae435e34660`,
    /// used by Chrome 150. Names match Dawn's Vulkan diagnostics or the Vulkan property it checks.
    pub(super) fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        let feature = |index: usize| self.features.bits[index] != VK_FALSE;
        for (index, name) in [
            (0, "robustBufferAccess"),
            (22, "textureCompressionBC"),
            (33, "shaderUniformBufferArrayDynamicIndexing"),
            (34, "shaderSampledImageArrayDynamicIndexing"),
            (35, "shaderStorageBufferArrayDynamicIndexing"),
            (36, "shaderStorageImageArrayDynamicIndexing"),
            (12, "depthBiasClamp"),
            (26, "fragmentStoresAndAtomics"),
            (1, "fullDrawIndexUint32"),
            (2, "imageCubeArray"),
            (3, "independentBlend"),
            (6, "sampleRateShading"),
        ] {
            if !feature(index) {
                missing.push(name);
            }
        }

        let limits = self.limits;
        let mut at_least = |actual, required, name| {
            if actual < required {
                missing.push(name);
            }
        };
        at_least(
            limits.max_image_dimension_1d as u64,
            8192,
            "maxImageDimension1D",
        );
        at_least(
            limits.max_image_dimension_2d as u64,
            8192,
            "maxImageDimension2D",
        );
        at_least(
            limits.max_image_dimension_cube as u64,
            8192,
            "maxImageDimensionCube",
        );
        at_least(
            limits.max_image_dimension_3d as u64,
            2048,
            "maxImageDimension3D",
        );
        at_least(
            limits.max_image_array_layers as u64,
            256,
            "maxImageArrayLayers",
        );
        at_least(
            limits.max_bound_descriptor_sets as u64,
            4,
            "maxBoundDescriptorSets",
        );
        at_least(
            limits.max_descriptor_set_uniform_buffers_dynamic as u64,
            8,
            "maxDescriptorSetUniformBuffersDynamic",
        );
        at_least(
            limits.max_descriptor_set_storage_buffers_dynamic as u64,
            4,
            "maxDescriptorSetStorageBuffersDynamic",
        );
        at_least(
            limits.max_per_stage_descriptor_sampled_images as u64,
            16,
            "maxPerStageDescriptorSampledImages",
        );
        at_least(
            limits.max_per_stage_descriptor_samplers as u64,
            16,
            "maxPerStageDescriptorSamplers",
        );
        at_least(
            limits.max_per_stage_descriptor_storage_buffers as u64,
            8,
            "maxPerStageDescriptorStorageBuffers",
        );
        at_least(
            limits.max_per_stage_descriptor_storage_images as u64,
            4,
            "maxPerStageDescriptorStorageImages",
        );
        at_least(
            limits.max_per_stage_descriptor_uniform_buffers as u64,
            12,
            "maxPerStageDescriptorUniformBuffers",
        );
        at_least(
            limits.max_storage_buffer_range as u64,
            128 << 20,
            "maxStorageBufferRange",
        );
        at_least(
            limits.max_uniform_buffer_range as u64,
            64 << 10,
            "maxUniformBufferRange",
        );
        at_least(
            limits.max_color_attachments as u64,
            8,
            "maxColorAttachments",
        );
        at_least(
            limits.max_vertex_input_bindings as u64,
            8,
            "maxVertexInputBindings",
        );
        at_least(
            limits.max_vertex_input_attributes as u64,
            16,
            "maxVertexInputAttributes",
        );
        at_least(
            limits.max_vertex_input_binding_stride as u64,
            2048,
            "maxVertexInputBindingStride",
        );
        at_least(
            limits.max_vertex_input_attribute_offset as u64,
            2047,
            "maxVertexInputAttributeOffset",
        );
        at_least(
            limits.max_vertex_output_components as u64,
            72,
            "maxVertexOutputComponents",
        );
        at_least(
            limits.max_fragment_input_components as u64,
            72,
            "maxFragmentInputComponents",
        );
        at_least(
            limits.max_compute_shared_memory_size as u64,
            16 << 10,
            "maxComputeSharedMemorySize",
        );
        at_least(
            limits.max_compute_work_group_invocations as u64,
            256,
            "maxComputeWorkGroupInvocations",
        );
        for (actual, required, name) in [
            (
                limits.max_compute_work_group_size[0] as u64,
                256,
                "maxComputeWorkGroupSizeX",
            ),
            (
                limits.max_compute_work_group_size[1] as u64,
                256,
                "maxComputeWorkGroupSizeY",
            ),
            (
                limits.max_compute_work_group_size[2] as u64,
                64,
                "maxComputeWorkGroupSizeZ",
            ),
        ] {
            at_least(actual, required, name);
        }
        for (axis, actual) in limits.max_compute_work_group_count.iter().enumerate() {
            if *actual < 65_535 {
                missing.push(match axis {
                    0 => "maxComputeWorkGroupCountX",
                    1 => "maxComputeWorkGroupCountY",
                    _ => "maxComputeWorkGroupCountZ",
                });
            }
        }
        if limits.min_uniform_buffer_offset_alignment > 256 {
            missing.push("minUniformBufferOffsetAlignment");
        }
        if limits.min_storage_buffer_offset_alignment > 256 {
            missing.push("minStorageBufferOffsetAlignment");
        }
        if limits.framebuffer_color_sample_counts & SAMPLE_COUNT_1_AND_4 != SAMPLE_COUNT_1_AND_4 {
            missing.push("framebufferColorSampleCounts");
        }
        if limits.framebuffer_depth_sample_counts & SAMPLE_COUNT_1_AND_4 != SAMPLE_COUNT_1_AND_4 {
            missing.push("framebufferDepthSampleCounts");
        }
        if self.max_buffer_size < 256 << 20 {
            missing.push("maxBufferSize");
        }
        missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::metal_limits;

    #[test]
    fn current_full_capabilities_meet_chrome_150_dawn_baseline() {
        let capabilities = hl_gpu::Capabilities::metal_class_fixture("test");
        let features = VkPhysicalDeviceFeatures::advertised(Some(&capabilities));
        let limits = metal_limits();

        assert_eq!(
            DawnBaseline::new(&features, &limits, 2 << 30).missing(),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn reports_every_missing_feature_by_dawn_name() {
        let features = VkPhysicalDeviceFeatures {
            bits: [VK_FALSE; 55],
        };
        let limits = metal_limits();
        let missing = DawnBaseline::new(&features, &limits, 2 << 30).missing();

        assert_eq!(
            &missing[..12],
            [
                "robustBufferAccess",
                "textureCompressionBC",
                "shaderUniformBufferArrayDynamicIndexing",
                "shaderSampledImageArrayDynamicIndexing",
                "shaderStorageBufferArrayDynamicIndexing",
                "shaderStorageImageArrayDynamicIndexing",
                "depthBiasClamp",
                "fragmentStoresAndAtomics",
                "fullDrawIndexUint32",
                "imageCubeArray",
                "independentBlend",
                "sampleRateShading",
            ]
        );
    }

    #[test]
    fn reports_exact_limit_failures_without_enabling_features() {
        let capabilities = hl_gpu::Capabilities::metal_class_fixture("test");
        let features = VkPhysicalDeviceFeatures::advertised(Some(&capabilities));
        let mut limits = metal_limits();
        limits.max_image_dimension_2d = 4096;
        limits.min_storage_buffer_offset_alignment = 512;
        limits.framebuffer_depth_sample_counts = 0x1;

        assert_eq!(
            DawnBaseline::new(&features, &limits, (256 << 20) - 1).missing(),
            [
                "maxImageDimension2D",
                "minStorageBufferOffsetAlignment",
                "framebufferDepthSampleCounts",
                "maxBufferSize",
            ]
        );
    }
}
