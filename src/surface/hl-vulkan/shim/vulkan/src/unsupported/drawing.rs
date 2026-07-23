#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksEXT(
    commandBuffer: *mut core::ffi::c_void,
    groupCountX: u32,
    groupCountY: u32,
    groupCountZ: u32,
) {
    let _ = commandBuffer;
    let _ = groupCountX;
    let _ = groupCountY;
    let _ = groupCountZ;
    crate::stub::Call::unsupported("vkCmdDrawMeshTasksEXT", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectCountEXT(
    commandBuffer: *mut core::ffi::c_void,
    buffer: u64,
    offset: u64,
    countBuffer: u64,
    countBufferOffset: u64,
    maxDrawCount: u32,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = buffer;
    let _ = offset;
    let _ = countBuffer;
    let _ = countBufferOffset;
    let _ = maxDrawCount;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkCmdDrawMeshTasksIndirectCountEXT",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectCountNV(
    commandBuffer: *mut core::ffi::c_void,
    buffer: u64,
    offset: u64,
    countBuffer: u64,
    countBufferOffset: u64,
    maxDrawCount: u32,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = buffer;
    let _ = offset;
    let _ = countBuffer;
    let _ = countBufferOffset;
    let _ = maxDrawCount;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkCmdDrawMeshTasksIndirectCountNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectEXT(
    commandBuffer: *mut core::ffi::c_void,
    buffer: u64,
    offset: u64,
    drawCount: u32,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = buffer;
    let _ = offset;
    let _ = drawCount;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkCmdDrawMeshTasksIndirectEXT",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectNV(
    commandBuffer: *mut core::ffi::c_void,
    buffer: u64,
    offset: u64,
    drawCount: u32,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = buffer;
    let _ = offset;
    let _ = drawCount;
    let _ = stride;
    crate::stub::Call::unsupported(
        "vkCmdDrawMeshTasksIndirectNV",
        "extension family not modeled",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksNV(
    commandBuffer: *mut core::ffi::c_void,
    taskCount: u32,
    firstTask: u32,
) {
    let _ = commandBuffer;
    let _ = taskCount;
    let _ = firstTask;
    crate::stub::Call::unsupported("vkCmdDrawMeshTasksNV", "extension family not modeled");
}

#[no_mangle]
pub extern "C" fn vkCmdBeginConditionalRenderingEXT(
    commandBuffer: *mut core::ffi::c_void,
    pConditionalRenderingBegin: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = pConditionalRenderingBegin;
    crate::stub::Call::unsupported(
        "vkCmdBeginConditionalRenderingEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdBeginQueryIndexedEXT(
    commandBuffer: *mut core::ffi::c_void,
    queryPool: u64,
    query: u32,
    flags: u32,
    index: u32,
) {
    let _ = commandBuffer;
    let _ = queryPool;
    let _ = query;
    let _ = flags;
    let _ = index;
    crate::stub::Call::unsupported("vkCmdBeginQueryIndexedEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdBeginTransformFeedbackEXT(
    commandBuffer: *mut core::ffi::c_void,
    firstCounterBuffer: u32,
    counterBufferCount: u32,
    pCounterBuffers: *const core::ffi::c_void,
    pCounterBufferOffsets: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = firstCounterBuffer;
    let _ = counterBufferCount;
    let _ = pCounterBuffers;
    let _ = pCounterBufferOffsets;
    crate::stub::Call::unsupported("vkCmdBeginTransformFeedbackEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdDrawClusterHUAWEI(
    commandBuffer: *mut core::ffi::c_void,
    groupCountX: u32,
    groupCountY: u32,
    groupCountZ: u32,
) {
    let _ = commandBuffer;
    let _ = groupCountX;
    let _ = groupCountY;
    let _ = groupCountZ;
    crate::stub::Call::unsupported("vkCmdDrawClusterHUAWEI", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdDrawClusterIndirectHUAWEI(
    commandBuffer: *mut core::ffi::c_void,
    buffer: u64,
    offset: u64,
) {
    let _ = commandBuffer;
    let _ = buffer;
    let _ = offset;
    crate::stub::Call::unsupported("vkCmdDrawClusterIndirectHUAWEI", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdDrawIndirectByteCountEXT(
    commandBuffer: *mut core::ffi::c_void,
    instanceCount: u32,
    firstInstance: u32,
    counterBuffer: u64,
    counterBufferOffset: u64,
    counterOffset: u32,
    vertexStride: u32,
) {
    let _ = commandBuffer;
    let _ = instanceCount;
    let _ = firstInstance;
    let _ = counterBuffer;
    let _ = counterBufferOffset;
    let _ = counterOffset;
    let _ = vertexStride;
    crate::stub::Call::unsupported("vkCmdDrawIndirectByteCountEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMultiEXT(
    commandBuffer: *mut core::ffi::c_void,
    drawCount: u32,
    pVertexInfo: *const core::ffi::c_void,
    instanceCount: u32,
    firstInstance: u32,
    stride: u32,
) {
    let _ = commandBuffer;
    let _ = drawCount;
    let _ = pVertexInfo;
    let _ = instanceCount;
    let _ = firstInstance;
    let _ = stride;
    crate::stub::Call::unsupported("vkCmdDrawMultiEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdDrawMultiIndexedEXT(
    commandBuffer: *mut core::ffi::c_void,
    drawCount: u32,
    pIndexInfo: *const core::ffi::c_void,
    instanceCount: u32,
    firstInstance: u32,
    stride: u32,
    pVertexOffset: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = drawCount;
    let _ = pIndexInfo;
    let _ = instanceCount;
    let _ = firstInstance;
    let _ = stride;
    let _ = pVertexOffset;
    crate::stub::Call::unsupported("vkCmdDrawMultiIndexedEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdEndConditionalRenderingEXT(commandBuffer: *mut core::ffi::c_void) {
    let _ = commandBuffer;
    crate::stub::Call::unsupported(
        "vkCmdEndConditionalRenderingEXT",
        "extension not advertised",
    );
}

#[no_mangle]
pub extern "C" fn vkCmdEndQueryIndexedEXT(
    commandBuffer: *mut core::ffi::c_void,
    queryPool: u64,
    query: u32,
    index: u32,
) {
    let _ = commandBuffer;
    let _ = queryPool;
    let _ = query;
    let _ = index;
    crate::stub::Call::unsupported("vkCmdEndQueryIndexedEXT", "extension not advertised");
}

#[no_mangle]
pub extern "C" fn vkCmdEndTransformFeedbackEXT(
    commandBuffer: *mut core::ffi::c_void,
    firstCounterBuffer: u32,
    counterBufferCount: u32,
    pCounterBuffers: *const core::ffi::c_void,
    pCounterBufferOffsets: *const core::ffi::c_void,
) {
    let _ = commandBuffer;
    let _ = firstCounterBuffer;
    let _ = counterBufferCount;
    let _ = pCounterBuffers;
    let _ = pCounterBufferOffsets;
    crate::stub::Call::unsupported("vkCmdEndTransformFeedbackEXT", "extension not advertised");
}
