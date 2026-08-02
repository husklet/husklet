use super::*;

const MAX_COMPUTE_WORKGROUP_SUBGROUPS: u32 = 32;
const MAX_INLINE_UNIFORM_BLOCK_SIZE: u32 = 256;
const MAX_INLINE_UNIFORM_BLOCKS: u32 = 4;
const MAX_INLINE_UNIFORM_TOTAL_SIZE: u32 = 256;
const TEXEL_BUFFER_OFFSET_ALIGNMENT: VkDeviceSize = 16;

/// Fill one property structure promoted into Vulkan 1.3. Returning `true` keeps the outer pNext walker
/// exhaustive without duplicating the aggregate's values across branches.
pub(super) fn try_fill(node: *mut VkBaseOutStructure) -> bool {
    let Some(header) = (unsafe { node.as_mut() }) else {
        return false;
    };
    match header.s_type {
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SUBGROUP_SIZE_CONTROL_PROPERTIES => {
            let out = unsafe { &mut *(node as *mut VkPhysicalDeviceSubgroupSizeControlProperties) };
            out.min_subgroup_size = Identity::SUBGROUP_SIZE;
            out.max_subgroup_size = Identity::SUBGROUP_SIZE;
            out.max_compute_workgroup_subgroups = MAX_COMPUTE_WORKGROUP_SUBGROUPS;
            out.required_subgroup_size_stages = 0;
        }
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_INLINE_UNIFORM_BLOCK_PROPERTIES => {
            let out = unsafe { &mut *(node as *mut VkPhysicalDeviceInlineUniformBlockProperties) };
            out.max_inline_uniform_block_size = MAX_INLINE_UNIFORM_BLOCK_SIZE;
            out.max_per_stage_descriptor_inline_uniform_blocks = MAX_INLINE_UNIFORM_BLOCKS;
            out.max_per_stage_descriptor_update_after_bind_inline_uniform_blocks =
                MAX_INLINE_UNIFORM_BLOCKS;
            out.max_descriptor_set_inline_uniform_blocks = MAX_INLINE_UNIFORM_BLOCKS;
            out.max_descriptor_set_update_after_bind_inline_uniform_blocks =
                MAX_INLINE_UNIFORM_BLOCKS;
        }
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SHADER_INTEGER_DOT_PRODUCT_PROPERTIES => {
            let out =
                unsafe { &mut *(node as *mut VkPhysicalDeviceShaderIntegerDotProductProperties) };
            out.accelerated = [VK_FALSE; 30];
        }
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TEXEL_BUFFER_ALIGNMENT_PROPERTIES => {
            let out =
                unsafe { &mut *(node as *mut VkPhysicalDeviceTexelBufferAlignmentProperties) };
            out.storage_texel_buffer_offset_alignment_bytes = TEXEL_BUFFER_OFFSET_ALIGNMENT;
            out.storage_texel_buffer_offset_single_texel_alignment = VK_FALSE;
            out.uniform_texel_buffer_offset_alignment_bytes = TEXEL_BUFFER_OFFSET_ALIGNMENT;
            out.uniform_texel_buffer_offset_single_texel_alignment = VK_FALSE;
        }
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_PROPERTIES => {
            let out = unsafe { &mut *(node as *mut VkPhysicalDeviceVulkan13Properties) };
            out.min_subgroup_size = Identity::SUBGROUP_SIZE;
            out.max_subgroup_size = Identity::SUBGROUP_SIZE;
            out.max_compute_workgroup_subgroups = MAX_COMPUTE_WORKGROUP_SUBGROUPS;
            // `subgroupSizeControl` is false, so no stage accepts an application-selected size.
            out.required_subgroup_size_stages = 0;
            // Required core-1.3 property minima do not enable the optional inline-uniform feature.
            out.max_inline_uniform_block_size = MAX_INLINE_UNIFORM_BLOCK_SIZE;
            out.max_per_stage_descriptor_inline_uniform_blocks = MAX_INLINE_UNIFORM_BLOCKS;
            out.max_per_stage_descriptor_update_after_bind_inline_uniform_blocks =
                MAX_INLINE_UNIFORM_BLOCKS;
            out.max_descriptor_set_inline_uniform_blocks = MAX_INLINE_UNIFORM_BLOCKS;
            out.max_descriptor_set_update_after_bind_inline_uniform_blocks =
                MAX_INLINE_UNIFORM_BLOCKS;
            out.max_inline_uniform_total_size = MAX_INLINE_UNIFORM_TOTAL_SIZE;
            out.integer_dot_product_accelerated = [VK_FALSE; 30];
            // Match the base `minTexelBufferOffsetAlignment`; no single-texel relaxation is claimed.
            out.storage_texel_buffer_offset_alignment_bytes = TEXEL_BUFFER_OFFSET_ALIGNMENT;
            out.storage_texel_buffer_offset_single_texel_alignment = VK_FALSE;
            out.uniform_texel_buffer_offset_alignment_bytes = TEXEL_BUFFER_OFFSET_ALIGNMENT;
            out.uniform_texel_buffer_offset_single_texel_alignment = VK_FALSE;
            out.max_buffer_size = StateStore::with(|s| s.physical_device().limits.max_buffer_size);
        }
        _ => return false,
    }
    true
}
