use super::*;

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBufferEmbeddedSamplers2EXT(
    commandBuffer: *mut core::ffi::c_void,
    pBindDescriptorBufferEmbeddedSamplersInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pBindDescriptorBufferEmbeddedSamplersInfo;
    crate::stub::Call::unsupported(
        "vkCmdBindDescriptorBufferEmbeddedSamplers2EXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBufferEmbeddedSamplersEXT(
    commandBuffer: *mut core::ffi::c_void,
    pipelineBindPoint: i32,
    layout: u64,
    set: u32,
) {
    let _ = commandBuffer;
    let _ = pipelineBindPoint;
    let _ = layout;
    let _ = set;
    crate::stub::Call::unsupported(
        "vkCmdBindDescriptorBufferEmbeddedSamplersEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBuffersEXT(
    commandBuffer: *mut core::ffi::c_void,
    bufferCount: u32,
    pBindingInfos: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = bufferCount;
    let _ = pBindingInfos;
    crate::stub::Call::unsupported("vkCmdBindDescriptorBuffersEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2(
    commandBuffer: *mut core::ffi::c_void,
    pBindDescriptorSetsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pBindDescriptorSetsInfo;
    crate::stub::Call::unsupported("vkCmdBindDescriptorSets2", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2KHR(
    commandBuffer: *mut core::ffi::c_void,
    pBindDescriptorSetsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pBindDescriptorSetsInfo;
    crate::stub::Call::unsupported("vkCmdBindDescriptorSets2KHR", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2(
    commandBuffer: *mut core::ffi::c_void,
    pPushConstantsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pPushConstantsInfo;
    crate::stub::Call::unsupported("vkCmdPushConstants2", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2KHR(
    commandBuffer: *mut core::ffi::c_void,
    pPushConstantsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pPushConstantsInfo;
    crate::stub::Call::unsupported("vkCmdPushConstants2KHR", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet(
    commandBuffer: *mut core::ffi::c_void,
    pipelineBindPoint: i32,
    layout: u64,
    set: u32,
    descriptorWriteCount: u32,
    pDescriptorWrites: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pipelineBindPoint;
    let _ = layout;
    let _ = set;
    let _ = descriptorWriteCount;
    let _ = pDescriptorWrites;
    crate::stub::Call::unsupported("vkCmdPushDescriptorSet", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2(
    commandBuffer: *mut core::ffi::c_void,
    pPushDescriptorSetInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pPushDescriptorSetInfo;
    crate::stub::Call::unsupported("vkCmdPushDescriptorSet2", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2KHR(
    commandBuffer: *mut core::ffi::c_void,
    pPushDescriptorSetInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pPushDescriptorSetInfo;
    crate::stub::Call::unsupported("vkCmdPushDescriptorSet2KHR", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetKHR(
    commandBuffer: *mut core::ffi::c_void,
    pipelineBindPoint: i32,
    layout: u64,
    set: u32,
    descriptorWriteCount: u32,
    pDescriptorWrites: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pipelineBindPoint;
    let _ = layout;
    let _ = set;
    let _ = descriptorWriteCount;
    let _ = pDescriptorWrites;
    crate::stub::Call::unsupported("vkCmdPushDescriptorSetKHR", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate(
    commandBuffer: *mut core::ffi::c_void,
    descriptorUpdateTemplate: u64,
    layout: u64,
    set: u32,
    pData: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = descriptorUpdateTemplate;
    let _ = layout;
    let _ = set;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkCmdPushDescriptorSetWithTemplate",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2(
    commandBuffer: *mut core::ffi::c_void,
    pPushDescriptorSetWithTemplateInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pPushDescriptorSetWithTemplateInfo;
    crate::stub::Call::unsupported(
        "vkCmdPushDescriptorSetWithTemplate2",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2KHR(
    commandBuffer: *mut core::ffi::c_void,
    pPushDescriptorSetWithTemplateInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pPushDescriptorSetWithTemplateInfo;
    crate::stub::Call::unsupported(
        "vkCmdPushDescriptorSetWithTemplate2KHR",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplateKHR(
    commandBuffer: *mut core::ffi::c_void,
    descriptorUpdateTemplate: u64,
    layout: u64,
    set: u32,
    pData: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = descriptorUpdateTemplate;
    let _ = layout;
    let _ = set;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkCmdPushDescriptorSetWithTemplateKHR",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdSetDescriptorBufferOffsets2EXT(
    commandBuffer: *mut core::ffi::c_void,
    pSetDescriptorBufferOffsetsInfo: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pSetDescriptorBufferOffsetsInfo;
    crate::stub::Call::unsupported(
        "vkCmdSetDescriptorBufferOffsets2EXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdSetDescriptorBufferOffsetsEXT(
    commandBuffer: *mut core::ffi::c_void,
    pipelineBindPoint: i32,
    layout: u64,
    firstSet: u32,
    setCount: u32,
    pBufferIndices: *const core::ffi::c_void,
    pOffsets: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pipelineBindPoint;
    let _ = layout;
    let _ = firstSet;
    let _ = setCount;
    let _ = pBufferIndices;
    let _ = pOffsets;
    crate::stub::Call::unsupported(
        "vkCmdSetDescriptorBufferOffsetsEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureDescriptorDataEXT(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetBufferOpaqueCaptureDescriptorDataEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetDescriptorEXT(
    device: *mut core::ffi::c_void,
    pDescriptorInfo: *const core::ffi::c_void,
    dataSize: usize,
    pDescriptor: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pDescriptorInfo;
    let _ = dataSize;
    let _ = pDescriptor;
    crate::stub::Call::unsupported("vkGetDescriptorEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetHostMappingVALVE(
    device: *mut core::ffi::c_void,
    descriptorSet: u64,
    ppData: *mut *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = descriptorSet;
    let _ = ppData;
    crate::stub::Call::unsupported(
        "vkGetDescriptorSetHostMappingVALVE",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutBindingOffsetEXT(
    device: *mut core::ffi::c_void,
    layout: u64,
    binding: u32,
    pOffset: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = layout;
    let _ = binding;
    let _ = pOffset;
    crate::stub::Call::unsupported(
        "vkGetDescriptorSetLayoutBindingOffsetEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutHostMappingInfoVALVE(
    device: *mut core::ffi::c_void,
    pBindingReference: *const core::ffi::c_void,
    pHostMapping: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = pBindingReference;
    let _ = pHostMapping;
    crate::stub::Call::unsupported(
        "vkGetDescriptorSetLayoutHostMappingInfoVALVE",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutSizeEXT(
    device: *mut core::ffi::c_void,
    layout: u64,
    pLayoutSizeInBytes: *mut core::ffi::c_void,
) {
    let _ = device;
    let _ = layout;
    let _ = pLayoutSizeInBytes;
    crate::stub::Call::unsupported(
        "vkGetDescriptorSetLayoutSizeEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkGetImageOpaqueCaptureDescriptorDataEXT(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetImageOpaqueCaptureDescriptorDataEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetImageViewOpaqueCaptureDescriptorDataEXT(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetImageViewOpaqueCaptureDescriptorDataEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}

#[no_mangle]
pub extern "C" fn vkGetSamplerOpaqueCaptureDescriptorDataEXT(
    device: *mut core::ffi::c_void,
    pInfo: *const core::ffi::c_void,
    pData: *mut core::ffi::c_void,
) -> i32 {
    let _ = device;
    let _ = pInfo;
    let _ = pData;
    crate::stub::Call::unsupported(
        "vkGetSamplerOpaqueCaptureDescriptorDataEXT",
        "extension not advertised",
    );
    VK_ERROR_EXTENSION_NOT_PRESENT
}
