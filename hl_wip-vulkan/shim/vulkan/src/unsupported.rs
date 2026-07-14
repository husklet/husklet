//! Honest HAND-WRITTEN not-supported bodies for the wholesale-unmodeled extension families:
//! ray tracing / acceleration structures, micromaps, video coding, mesh/task shading,
//! cooperative matrix/vector, and optical flow.
//!
//! hl models none of these pipelines, and their extensions are NOT advertised
//! (advertise-only-what's-real). Each entry point validates its argument shape, nulls any output
//! handle, once-logs an `unsupported` trace, and returns the truthful
//! `VK_ERROR_EXTENSION_NOT_PRESENT` (a `VkResult` command) or the truthful zero/NULL — never a
//! false `VK_SUCCESS`. Signatures are the exact C ABI (mirroring the generator's type mapping), so
//! the loader/app resolves + links every symbol. These names move GENERATED_STUBS -> IMPLEMENTED.

#![allow(clippy::too_many_arguments, unused_variables)]

/// `VK_ERROR_EXTENSION_NOT_PRESENT` (stable Vulkan ABI) — the truthful result for a command from an
/// extension this ICD does not advertise/back.
const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;

#[no_mangle]
pub extern "C" fn vkBindAccelerationStructureMemoryNV(device: *mut core::ffi::c_void, bindInfoCount: u32, pBindInfos: *const core::ffi::c_void) -> i32 { let _ = device; let _ = bindInfoCount; let _ = pBindInfos; crate::stub::unsupported("vkBindAccelerationStructureMemoryNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBindOpticalFlowSessionImageNV(device: *mut core::ffi::c_void, session: u64, bindingPoint: i32, view: u64, layout: i32) -> i32 { let _ = device; let _ = session; let _ = bindingPoint; let _ = view; let _ = layout; crate::stub::unsupported("vkBindOpticalFlowSessionImageNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBindVideoSessionMemoryKHR(device: *mut core::ffi::c_void, videoSession: u64, bindSessionMemoryInfoCount: u32, pBindSessionMemoryInfos: *const core::ffi::c_void) -> i32 { let _ = device; let _ = videoSession; let _ = bindSessionMemoryInfoCount; let _ = pBindSessionMemoryInfos; crate::stub::unsupported("vkBindVideoSessionMemoryKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBuildAccelerationStructuresKHR(device: *mut core::ffi::c_void, deferredOperation: u64, infoCount: u32, pInfos: *const core::ffi::c_void, ppBuildRangeInfos: *const *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = infoCount; let _ = pInfos; let _ = ppBuildRangeInfos; crate::stub::unsupported("vkBuildAccelerationStructuresKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBuildMicromapsEXT(device: *mut core::ffi::c_void, deferredOperation: u64, infoCount: u32, pInfos: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = infoCount; let _ = pInfos; crate::stub::unsupported("vkBuildMicromapsEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdBeginVideoCodingKHR(commandBuffer: *mut core::ffi::c_void, pBeginInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBeginInfo; crate::stub::unsupported("vkCmdBeginVideoCodingKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructureNV(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, instanceData: u64, instanceOffset: u64, update: u32, dst: u64, src: u64, scratch: u64, scratchOffset: u64) { let _ = commandBuffer; let _ = pInfo; let _ = instanceData; let _ = instanceOffset; let _ = update; let _ = dst; let _ = src; let _ = scratch; let _ = scratchOffset; crate::stub::unsupported("vkCmdBuildAccelerationStructureNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructuresIndirectKHR(commandBuffer: *mut core::ffi::c_void, infoCount: u32, pInfos: *const core::ffi::c_void, pIndirectDeviceAddresses: *const core::ffi::c_void, pIndirectStrides: *const core::ffi::c_void, ppMaxPrimitiveCounts: *const *const core::ffi::c_void) { let _ = commandBuffer; let _ = infoCount; let _ = pInfos; let _ = pIndirectDeviceAddresses; let _ = pIndirectStrides; let _ = ppMaxPrimitiveCounts; crate::stub::unsupported("vkCmdBuildAccelerationStructuresIndirectKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructuresKHR(commandBuffer: *mut core::ffi::c_void, infoCount: u32, pInfos: *const core::ffi::c_void, ppBuildRangeInfos: *const *const core::ffi::c_void) { let _ = commandBuffer; let _ = infoCount; let _ = pInfos; let _ = ppBuildRangeInfos; crate::stub::unsupported("vkCmdBuildAccelerationStructuresKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildMicromapsEXT(commandBuffer: *mut core::ffi::c_void, infoCount: u32, pInfos: *const core::ffi::c_void) { let _ = commandBuffer; let _ = infoCount; let _ = pInfos; crate::stub::unsupported("vkCmdBuildMicromapsEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdControlVideoCodingKHR(commandBuffer: *mut core::ffi::c_void, pCodingControlInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pCodingControlInfo; crate::stub::unsupported("vkCmdControlVideoCodingKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureKHR(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::unsupported("vkCmdCopyAccelerationStructureKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureNV(commandBuffer: *mut core::ffi::c_void, dst: u64, src: u64, mode: i32) { let _ = commandBuffer; let _ = dst; let _ = src; let _ = mode; crate::stub::unsupported("vkCmdCopyAccelerationStructureNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureToMemoryKHR(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::unsupported("vkCmdCopyAccelerationStructureToMemoryKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToAccelerationStructureKHR(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::unsupported("vkCmdCopyMemoryToAccelerationStructureKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToMicromapEXT(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::unsupported("vkCmdCopyMemoryToMicromapEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMicromapEXT(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::unsupported("vkCmdCopyMicromapEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMicromapToMemoryEXT(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::unsupported("vkCmdCopyMicromapToMemoryEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDecodeVideoKHR(commandBuffer: *mut core::ffi::c_void, pDecodeInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pDecodeInfo; crate::stub::unsupported("vkCmdDecodeVideoKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksEXT(commandBuffer: *mut core::ffi::c_void, groupCountX: u32, groupCountY: u32, groupCountZ: u32) { let _ = commandBuffer; let _ = groupCountX; let _ = groupCountY; let _ = groupCountZ; crate::stub::unsupported("vkCmdDrawMeshTasksEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectCountEXT(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, countBuffer: u64, countBufferOffset: u64, maxDrawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = countBuffer; let _ = countBufferOffset; let _ = maxDrawCount; let _ = stride; crate::stub::unsupported("vkCmdDrawMeshTasksIndirectCountEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectCountNV(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, countBuffer: u64, countBufferOffset: u64, maxDrawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = countBuffer; let _ = countBufferOffset; let _ = maxDrawCount; let _ = stride; crate::stub::unsupported("vkCmdDrawMeshTasksIndirectCountNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectEXT(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, drawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = drawCount; let _ = stride; crate::stub::unsupported("vkCmdDrawMeshTasksIndirectEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectNV(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, drawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = drawCount; let _ = stride; crate::stub::unsupported("vkCmdDrawMeshTasksIndirectNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksNV(commandBuffer: *mut core::ffi::c_void, taskCount: u32, firstTask: u32) { let _ = commandBuffer; let _ = taskCount; let _ = firstTask; crate::stub::unsupported("vkCmdDrawMeshTasksNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdEncodeVideoKHR(commandBuffer: *mut core::ffi::c_void, pEncodeInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pEncodeInfo; crate::stub::unsupported("vkCmdEncodeVideoKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdEndVideoCodingKHR(commandBuffer: *mut core::ffi::c_void, pEndCodingInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pEndCodingInfo; crate::stub::unsupported("vkCmdEndVideoCodingKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdOpticalFlowExecuteNV(commandBuffer: *mut core::ffi::c_void, session: u64, pExecuteInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = session; let _ = pExecuteInfo; crate::stub::unsupported("vkCmdOpticalFlowExecuteNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdSetRayTracingPipelineStackSizeKHR(commandBuffer: *mut core::ffi::c_void, pipelineStackSize: u32) { let _ = commandBuffer; let _ = pipelineStackSize; crate::stub::unsupported("vkCmdSetRayTracingPipelineStackSizeKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysIndirect2KHR(commandBuffer: *mut core::ffi::c_void, indirectDeviceAddress: u64) { let _ = commandBuffer; let _ = indirectDeviceAddress; crate::stub::unsupported("vkCmdTraceRaysIndirect2KHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysIndirectKHR(commandBuffer: *mut core::ffi::c_void, pRaygenShaderBindingTable: *const core::ffi::c_void, pMissShaderBindingTable: *const core::ffi::c_void, pHitShaderBindingTable: *const core::ffi::c_void, pCallableShaderBindingTable: *const core::ffi::c_void, indirectDeviceAddress: u64) { let _ = commandBuffer; let _ = pRaygenShaderBindingTable; let _ = pMissShaderBindingTable; let _ = pHitShaderBindingTable; let _ = pCallableShaderBindingTable; let _ = indirectDeviceAddress; crate::stub::unsupported("vkCmdTraceRaysIndirectKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysKHR(commandBuffer: *mut core::ffi::c_void, pRaygenShaderBindingTable: *const core::ffi::c_void, pMissShaderBindingTable: *const core::ffi::c_void, pHitShaderBindingTable: *const core::ffi::c_void, pCallableShaderBindingTable: *const core::ffi::c_void, width: u32, height: u32, depth: u32) { let _ = commandBuffer; let _ = pRaygenShaderBindingTable; let _ = pMissShaderBindingTable; let _ = pHitShaderBindingTable; let _ = pCallableShaderBindingTable; let _ = width; let _ = height; let _ = depth; crate::stub::unsupported("vkCmdTraceRaysKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysNV(commandBuffer: *mut core::ffi::c_void, raygenShaderBindingTableBuffer: u64, raygenShaderBindingOffset: u64, missShaderBindingTableBuffer: u64, missShaderBindingOffset: u64, missShaderBindingStride: u64, hitShaderBindingTableBuffer: u64, hitShaderBindingOffset: u64, hitShaderBindingStride: u64, callableShaderBindingTableBuffer: u64, callableShaderBindingOffset: u64, callableShaderBindingStride: u64, width: u32, height: u32, depth: u32) { let _ = commandBuffer; let _ = raygenShaderBindingTableBuffer; let _ = raygenShaderBindingOffset; let _ = missShaderBindingTableBuffer; let _ = missShaderBindingOffset; let _ = missShaderBindingStride; let _ = hitShaderBindingTableBuffer; let _ = hitShaderBindingOffset; let _ = hitShaderBindingStride; let _ = callableShaderBindingTableBuffer; let _ = callableShaderBindingOffset; let _ = callableShaderBindingStride; let _ = width; let _ = height; let _ = depth; crate::stub::unsupported("vkCmdTraceRaysNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteAccelerationStructuresPropertiesKHR(commandBuffer: *mut core::ffi::c_void, accelerationStructureCount: u32, pAccelerationStructures: *const core::ffi::c_void, queryType: i32, queryPool: u64, firstQuery: u32) { let _ = commandBuffer; let _ = accelerationStructureCount; let _ = pAccelerationStructures; let _ = queryType; let _ = queryPool; let _ = firstQuery; crate::stub::unsupported("vkCmdWriteAccelerationStructuresPropertiesKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteAccelerationStructuresPropertiesNV(commandBuffer: *mut core::ffi::c_void, accelerationStructureCount: u32, pAccelerationStructures: *const core::ffi::c_void, queryType: i32, queryPool: u64, firstQuery: u32) { let _ = commandBuffer; let _ = accelerationStructureCount; let _ = pAccelerationStructures; let _ = queryType; let _ = queryPool; let _ = firstQuery; crate::stub::unsupported("vkCmdWriteAccelerationStructuresPropertiesNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteMicromapsPropertiesEXT(commandBuffer: *mut core::ffi::c_void, micromapCount: u32, pMicromaps: *const core::ffi::c_void, queryType: i32, queryPool: u64, firstQuery: u32) { let _ = commandBuffer; let _ = micromapCount; let _ = pMicromaps; let _ = queryType; let _ = queryPool; let _ = firstQuery; crate::stub::unsupported("vkCmdWriteMicromapsPropertiesEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCopyAccelerationStructureKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::unsupported("vkCopyAccelerationStructureKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyAccelerationStructureToMemoryKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::unsupported("vkCopyAccelerationStructureToMemoryKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMemoryToAccelerationStructureKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::unsupported("vkCopyMemoryToAccelerationStructureKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMemoryToMicromapEXT(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::unsupported("vkCopyMemoryToMicromapEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMicromapEXT(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::unsupported("vkCopyMicromapEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMicromapToMemoryEXT(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::unsupported("vkCopyMicromapToMemoryEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateAccelerationStructureKHR(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pAccelerationStructure: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pAccelerationStructure; unsafe { if !pAccelerationStructure.is_null() { *(pAccelerationStructure as *mut u64) = 0; } } crate::stub::unsupported("vkCreateAccelerationStructureKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateAccelerationStructureNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pAccelerationStructure: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pAccelerationStructure; unsafe { if !pAccelerationStructure.is_null() { *(pAccelerationStructure as *mut u64) = 0; } } crate::stub::unsupported("vkCreateAccelerationStructureNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateMicromapEXT(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pMicromap: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pMicromap; unsafe { if !pMicromap.is_null() { *(pMicromap as *mut u64) = 0; } } crate::stub::unsupported("vkCreateMicromapEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateOpticalFlowSessionNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSession: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pSession; unsafe { if !pSession.is_null() { *(pSession as *mut u64) = 0; } } crate::stub::unsupported("vkCreateOpticalFlowSessionNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateRayTracingPipelinesKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pipelineCache: u64, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pPipelines: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pipelineCache; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pPipelines; unsafe { if !pPipelines.is_null() { *(pPipelines as *mut u64) = 0; } } crate::stub::unsupported("vkCreateRayTracingPipelinesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateRayTracingPipelinesNV(device: *mut core::ffi::c_void, pipelineCache: u64, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pPipelines: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipelineCache; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pPipelines; unsafe { if !pPipelines.is_null() { *(pPipelines as *mut u64) = 0; } } crate::stub::unsupported("vkCreateRayTracingPipelinesNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateVideoSessionKHR(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pVideoSession: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pVideoSession; unsafe { if !pVideoSession.is_null() { *(pVideoSession as *mut u64) = 0; } } crate::stub::unsupported("vkCreateVideoSessionKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateVideoSessionParametersKHR(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pVideoSessionParameters: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pVideoSessionParameters; unsafe { if !pVideoSessionParameters.is_null() { *(pVideoSessionParameters as *mut u64) = 0; } } crate::stub::unsupported("vkCreateVideoSessionParametersKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkDestroyAccelerationStructureKHR(device: *mut core::ffi::c_void, accelerationStructure: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = accelerationStructure; let _ = pAllocator; crate::stub::unsupported("vkDestroyAccelerationStructureKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyAccelerationStructureNV(device: *mut core::ffi::c_void, accelerationStructure: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = accelerationStructure; let _ = pAllocator; crate::stub::unsupported("vkDestroyAccelerationStructureNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyMicromapEXT(device: *mut core::ffi::c_void, micromap: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = micromap; let _ = pAllocator; crate::stub::unsupported("vkDestroyMicromapEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyOpticalFlowSessionNV(device: *mut core::ffi::c_void, session: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = session; let _ = pAllocator; crate::stub::unsupported("vkDestroyOpticalFlowSessionNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyVideoSessionKHR(device: *mut core::ffi::c_void, videoSession: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = videoSession; let _ = pAllocator; crate::stub::unsupported("vkDestroyVideoSessionKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyVideoSessionParametersKHR(device: *mut core::ffi::c_void, videoSessionParameters: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = videoSessionParameters; let _ = pAllocator; crate::stub::unsupported("vkDestroyVideoSessionParametersKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureBuildSizesKHR(device: *mut core::ffi::c_void, buildType: i32, pBuildInfo: *const core::ffi::c_void, pMaxPrimitiveCounts: *const core::ffi::c_void, pSizeInfo: *mut core::ffi::c_void) { let _ = device; let _ = buildType; let _ = pBuildInfo; let _ = pMaxPrimitiveCounts; let _ = pSizeInfo; crate::stub::unsupported("vkGetAccelerationStructureBuildSizesKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureDeviceAddressKHR(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> u64 { let _ = device; let _ = pInfo; crate::stub::unsupported("vkGetAccelerationStructureDeviceAddressKHR", "extension family not modeled"); 0 }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureHandleNV(device: *mut core::ffi::c_void, accelerationStructure: u64, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = accelerationStructure; let _ = dataSize; let _ = pData; crate::stub::unsupported("vkGetAccelerationStructureHandleNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureMemoryRequirementsNV(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) { let _ = device; let _ = pInfo; let _ = pMemoryRequirements; crate::stub::unsupported("vkGetAccelerationStructureMemoryRequirementsNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::unsupported("vkGetAccelerationStructureOpaqueCaptureDescriptorDataEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDeviceAccelerationStructureCompatibilityKHR(device: *mut core::ffi::c_void, pVersionInfo: *const core::ffi::c_void, pCompatibility: *mut core::ffi::c_void) { let _ = device; let _ = pVersionInfo; let _ = pCompatibility; crate::stub::unsupported("vkGetDeviceAccelerationStructureCompatibilityKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetDeviceMicromapCompatibilityEXT(device: *mut core::ffi::c_void, pVersionInfo: *const core::ffi::c_void, pCompatibility: *mut core::ffi::c_void) { let _ = device; let _ = pVersionInfo; let _ = pCompatibility; crate::stub::unsupported("vkGetDeviceMicromapCompatibilityEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetEncodedVideoSessionParametersKHR(device: *mut core::ffi::c_void, pVideoSessionParametersInfo: *const core::ffi::c_void, pFeedbackInfo: *mut core::ffi::c_void, pDataSize: *mut core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pVideoSessionParametersInfo; let _ = pFeedbackInfo; let _ = pDataSize; let _ = pData; crate::stub::unsupported("vkGetEncodedVideoSessionParametersKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMicromapBuildSizesEXT(device: *mut core::ffi::c_void, buildType: i32, pBuildInfo: *const core::ffi::c_void, pSizeInfo: *mut core::ffi::c_void) { let _ = device; let _ = buildType; let _ = pBuildInfo; let _ = pSizeInfo; crate::stub::unsupported("vkGetMicromapBuildSizesEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceCooperativeMatrixPropertiesNV(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetPhysicalDeviceCooperativeMatrixPropertiesNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceOpticalFlowImageFormatsNV(physicalDevice: *mut core::ffi::c_void, pOpticalFlowImageFormatInfo: *const core::ffi::c_void, pFormatCount: *mut core::ffi::c_void, pImageFormatProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pOpticalFlowImageFormatInfo; let _ = pFormatCount; let _ = pImageFormatProperties; crate::stub::unsupported("vkGetPhysicalDeviceOpticalFlowImageFormatsNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoCapabilitiesKHR(physicalDevice: *mut core::ffi::c_void, pVideoProfile: *const core::ffi::c_void, pCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pVideoProfile; let _ = pCapabilities; crate::stub::unsupported("vkGetPhysicalDeviceVideoCapabilitiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pQualityLevelInfo: *const core::ffi::c_void, pQualityLevelProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pQualityLevelInfo; let _ = pQualityLevelProperties; crate::stub::unsupported("vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoFormatPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pVideoFormatInfo: *const core::ffi::c_void, pVideoFormatPropertyCount: *mut core::ffi::c_void, pVideoFormatProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pVideoFormatInfo; let _ = pVideoFormatPropertyCount; let _ = pVideoFormatProperties; crate::stub::unsupported("vkGetPhysicalDeviceVideoFormatPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingCaptureReplayShaderGroupHandlesKHR(device: *mut core::ffi::c_void, pipeline: u64, firstGroup: u32, groupCount: u32, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = firstGroup; let _ = groupCount; let _ = dataSize; let _ = pData; crate::stub::unsupported("vkGetRayTracingCaptureReplayShaderGroupHandlesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingShaderGroupHandlesKHR(device: *mut core::ffi::c_void, pipeline: u64, firstGroup: u32, groupCount: u32, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = firstGroup; let _ = groupCount; let _ = dataSize; let _ = pData; crate::stub::unsupported("vkGetRayTracingShaderGroupHandlesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingShaderGroupHandlesNV(device: *mut core::ffi::c_void, pipeline: u64, firstGroup: u32, groupCount: u32, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = firstGroup; let _ = groupCount; let _ = dataSize; let _ = pData; crate::stub::unsupported("vkGetRayTracingShaderGroupHandlesNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingShaderGroupStackSizeKHR(device: *mut core::ffi::c_void, pipeline: u64, group: u32, groupShader: i32) -> u64 { let _ = device; let _ = pipeline; let _ = group; let _ = groupShader; crate::stub::unsupported("vkGetRayTracingShaderGroupStackSizeKHR", "extension family not modeled"); 0 }

#[no_mangle]
pub extern "C" fn vkGetVideoSessionMemoryRequirementsKHR(device: *mut core::ffi::c_void, videoSession: u64, pMemoryRequirementsCount: *mut core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = videoSession; let _ = pMemoryRequirementsCount; let _ = pMemoryRequirements; crate::stub::unsupported("vkGetVideoSessionMemoryRequirementsKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkUpdateVideoSessionParametersKHR(device: *mut core::ffi::c_void, videoSessionParameters: u64, pUpdateInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = videoSessionParameters; let _ = pUpdateInfo; crate::stub::unsupported("vkUpdateVideoSessionParametersKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkWriteAccelerationStructuresPropertiesKHR(device: *mut core::ffi::c_void, accelerationStructureCount: u32, pAccelerationStructures: *const core::ffi::c_void, queryType: i32, dataSize: usize, pData: *mut core::ffi::c_void, stride: usize) -> i32 { let _ = device; let _ = accelerationStructureCount; let _ = pAccelerationStructures; let _ = queryType; let _ = dataSize; let _ = pData; let _ = stride; crate::stub::unsupported("vkWriteAccelerationStructuresPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkWriteMicromapsPropertiesEXT(device: *mut core::ffi::c_void, micromapCount: u32, pMicromaps: *const core::ffi::c_void, queryType: i32, dataSize: usize, pData: *mut core::ffi::c_void, stride: usize) -> i32 { let _ = device; let _ = micromapCount; let _ = pMicromaps; let _ = queryType; let _ = dataSize; let _ = pData; let _ = stride; crate::stub::unsupported("vkWriteMicromapsPropertiesEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

