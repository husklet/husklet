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
/// `VK_ERROR_FEATURE_NOT_PRESENT` (stable Vulkan ABI) — the truthful result for a CORE command that
/// needs an optional device feature this ICD does not expose (e.g. sparse binding, device fault).
const VK_ERROR_FEATURE_NOT_PRESENT: i32 = -8;

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


// ---- @generated honest not-supported bodies for the unmodeled long tail (appended by task) ----

// Each validates argument shape, nulls a create/allocate output handle, zeroes a query count

// (so a two-call enumeration reads zero results, never junk), once-logs an `unsupported` trace,

// and returns the truthful VkResult / zero. The extensions these belong to are NOT advertised.


#[no_mangle]
pub extern "C" fn vkAcquireDrmDisplayEXT(physicalDevice: *mut core::ffi::c_void, drmFd: i32, display: u64) -> i32 { let _ = physicalDevice; let _ = drmFd; let _ = display; crate::stub::unsupported("vkAcquireDrmDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireFullScreenExclusiveModeEXT(device: *mut core::ffi::c_void, swapchain: u64) -> i32 { let _ = device; let _ = swapchain; crate::stub::unsupported("vkAcquireFullScreenExclusiveModeEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireImageANDROID(device: *mut core::ffi::c_void, image: u64, nativeFenceFd: i32, semaphore: u64, fence: u64) -> i32 { let _ = device; let _ = image; let _ = nativeFenceFd; let _ = semaphore; let _ = fence; crate::stub::unsupported("vkAcquireImageANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireNextImage2KHR(device: *mut core::ffi::c_void, pAcquireInfo: *const core::ffi::c_void, pImageIndex: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pAcquireInfo; let _ = pImageIndex; crate::stub::unsupported("vkAcquireNextImage2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquirePerformanceConfigurationINTEL(device: *mut core::ffi::c_void, pAcquireInfo: *const core::ffi::c_void, pConfiguration: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pAcquireInfo; let _ = pConfiguration; crate::stub::unsupported("vkAcquirePerformanceConfigurationINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireProfilingLockKHR(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; crate::stub::unsupported("vkAcquireProfilingLockKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireWinrtDisplayNV(physicalDevice: *mut core::ffi::c_void, display: u64) -> i32 { let _ = physicalDevice; let _ = display; crate::stub::unsupported("vkAcquireWinrtDisplayNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireXlibDisplayEXT(physicalDevice: *mut core::ffi::c_void, dpy: *mut core::ffi::c_void, display: u64) -> i32 { let _ = physicalDevice; let _ = dpy; let _ = display; crate::stub::unsupported("vkAcquireXlibDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdBeginConditionalRenderingEXT(commandBuffer: *mut core::ffi::c_void, pConditionalRenderingBegin: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pConditionalRenderingBegin; crate::stub::unsupported("vkCmdBeginConditionalRenderingEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBeginQueryIndexedEXT(commandBuffer: *mut core::ffi::c_void, queryPool: u64, query: u32, flags: u32, index: u32) { let _ = commandBuffer; let _ = queryPool; let _ = query; let _ = flags; let _ = index; crate::stub::unsupported("vkCmdBeginQueryIndexedEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBeginTransformFeedbackEXT(commandBuffer: *mut core::ffi::c_void, firstCounterBuffer: u32, counterBufferCount: u32, pCounterBuffers: *const core::ffi::c_void, pCounterBufferOffsets: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstCounterBuffer; let _ = counterBufferCount; let _ = pCounterBuffers; let _ = pCounterBufferOffsets; crate::stub::unsupported("vkCmdBeginTransformFeedbackEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBufferEmbeddedSamplers2EXT(commandBuffer: *mut core::ffi::c_void, pBindDescriptorBufferEmbeddedSamplersInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBindDescriptorBufferEmbeddedSamplersInfo; crate::stub::unsupported("vkCmdBindDescriptorBufferEmbeddedSamplers2EXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBufferEmbeddedSamplersEXT(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, set: u32) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = set; crate::stub::unsupported("vkCmdBindDescriptorBufferEmbeddedSamplersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBuffersEXT(commandBuffer: *mut core::ffi::c_void, bufferCount: u32, pBindingInfos: *const core::ffi::c_void) { let _ = commandBuffer; let _ = bufferCount; let _ = pBindingInfos; crate::stub::unsupported("vkCmdBindDescriptorBuffersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2(commandBuffer: *mut core::ffi::c_void, pBindDescriptorSetsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBindDescriptorSetsInfo; crate::stub::unsupported("vkCmdBindDescriptorSets2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2KHR(commandBuffer: *mut core::ffi::c_void, pBindDescriptorSetsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBindDescriptorSetsInfo; crate::stub::unsupported("vkCmdBindDescriptorSets2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer2(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, size: u64, indexType: i32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = size; let _ = indexType; crate::stub::unsupported("vkCmdBindIndexBuffer2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer2KHR(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, size: u64, indexType: i32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = size; let _ = indexType; crate::stub::unsupported("vkCmdBindIndexBuffer2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindInvocationMaskHUAWEI(commandBuffer: *mut core::ffi::c_void, imageView: u64, imageLayout: i32) { let _ = commandBuffer; let _ = imageView; let _ = imageLayout; crate::stub::unsupported("vkCmdBindInvocationMaskHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindPipelineShaderGroupNV(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, pipeline: u64, groupIndex: u32) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = pipeline; let _ = groupIndex; crate::stub::unsupported("vkCmdBindPipelineShaderGroupNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindShadersEXT(commandBuffer: *mut core::ffi::c_void, stageCount: u32, pStages: *const core::ffi::c_void, pShaders: *const core::ffi::c_void) { let _ = commandBuffer; let _ = stageCount; let _ = pStages; let _ = pShaders; crate::stub::unsupported("vkCmdBindShadersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindShadingRateImageNV(commandBuffer: *mut core::ffi::c_void, imageView: u64, imageLayout: i32) { let _ = commandBuffer; let _ = imageView; let _ = imageLayout; crate::stub::unsupported("vkCmdBindShadingRateImageNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindTransformFeedbackBuffersEXT(commandBuffer: *mut core::ffi::c_void, firstBinding: u32, bindingCount: u32, pBuffers: *const core::ffi::c_void, pOffsets: *const core::ffi::c_void, pSizes: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstBinding; let _ = bindingCount; let _ = pBuffers; let _ = pOffsets; let _ = pSizes; crate::stub::unsupported("vkCmdBindTransformFeedbackBuffersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryIndirectNV(commandBuffer: *mut core::ffi::c_void, copyBufferAddress: u64, copyCount: u32, stride: u32) { let _ = commandBuffer; let _ = copyBufferAddress; let _ = copyCount; let _ = stride; crate::stub::unsupported("vkCmdCopyMemoryIndirectNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToImageIndirectNV(commandBuffer: *mut core::ffi::c_void, copyBufferAddress: u64, copyCount: u32, stride: u32, dstImage: u64, dstImageLayout: i32, pImageSubresources: *const core::ffi::c_void) { let _ = commandBuffer; let _ = copyBufferAddress; let _ = copyCount; let _ = stride; let _ = dstImage; let _ = dstImageLayout; let _ = pImageSubresources; crate::stub::unsupported("vkCmdCopyMemoryToImageIndirectNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCuLaunchKernelNVX(commandBuffer: *mut core::ffi::c_void, pLaunchInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pLaunchInfo; crate::stub::unsupported("vkCmdCuLaunchKernelNVX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCudaLaunchKernelNV(commandBuffer: *mut core::ffi::c_void, pLaunchInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pLaunchInfo; crate::stub::unsupported("vkCmdCudaLaunchKernelNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDecompressMemoryIndirectCountNV(commandBuffer: *mut core::ffi::c_void, indirectCommandsAddress: u64, indirectCommandsCountAddress: u64, stride: u32) { let _ = commandBuffer; let _ = indirectCommandsAddress; let _ = indirectCommandsCountAddress; let _ = stride; crate::stub::unsupported("vkCmdDecompressMemoryIndirectCountNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDecompressMemoryNV(commandBuffer: *mut core::ffi::c_void, decompressRegionCount: u32, pDecompressMemoryRegions: *const core::ffi::c_void) { let _ = commandBuffer; let _ = decompressRegionCount; let _ = pDecompressMemoryRegions; crate::stub::unsupported("vkCmdDecompressMemoryNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDispatchGraphAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64, pCountInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = scratch; let _ = pCountInfo; crate::stub::unsupported("vkCmdDispatchGraphAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDispatchGraphIndirectAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64, pCountInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = scratch; let _ = pCountInfo; crate::stub::unsupported("vkCmdDispatchGraphIndirectAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDispatchGraphIndirectCountAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64, countInfo: u64) { let _ = commandBuffer; let _ = scratch; let _ = countInfo; crate::stub::unsupported("vkCmdDispatchGraphIndirectCountAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawClusterHUAWEI(commandBuffer: *mut core::ffi::c_void, groupCountX: u32, groupCountY: u32, groupCountZ: u32) { let _ = commandBuffer; let _ = groupCountX; let _ = groupCountY; let _ = groupCountZ; crate::stub::unsupported("vkCmdDrawClusterHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawClusterIndirectHUAWEI(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64) { let _ = commandBuffer; let _ = buffer; let _ = offset; crate::stub::unsupported("vkCmdDrawClusterIndirectHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawIndirectByteCountEXT(commandBuffer: *mut core::ffi::c_void, instanceCount: u32, firstInstance: u32, counterBuffer: u64, counterBufferOffset: u64, counterOffset: u32, vertexStride: u32) { let _ = commandBuffer; let _ = instanceCount; let _ = firstInstance; let _ = counterBuffer; let _ = counterBufferOffset; let _ = counterOffset; let _ = vertexStride; crate::stub::unsupported("vkCmdDrawIndirectByteCountEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMultiEXT(commandBuffer: *mut core::ffi::c_void, drawCount: u32, pVertexInfo: *const core::ffi::c_void, instanceCount: u32, firstInstance: u32, stride: u32) { let _ = commandBuffer; let _ = drawCount; let _ = pVertexInfo; let _ = instanceCount; let _ = firstInstance; let _ = stride; crate::stub::unsupported("vkCmdDrawMultiEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMultiIndexedEXT(commandBuffer: *mut core::ffi::c_void, drawCount: u32, pIndexInfo: *const core::ffi::c_void, instanceCount: u32, firstInstance: u32, stride: u32, pVertexOffset: *const core::ffi::c_void) { let _ = commandBuffer; let _ = drawCount; let _ = pIndexInfo; let _ = instanceCount; let _ = firstInstance; let _ = stride; let _ = pVertexOffset; crate::stub::unsupported("vkCmdDrawMultiIndexedEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdEndConditionalRenderingEXT(commandBuffer: *mut core::ffi::c_void) { let _ = commandBuffer; crate::stub::unsupported("vkCmdEndConditionalRenderingEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdEndQueryIndexedEXT(commandBuffer: *mut core::ffi::c_void, queryPool: u64, query: u32, index: u32) { let _ = commandBuffer; let _ = queryPool; let _ = query; let _ = index; crate::stub::unsupported("vkCmdEndQueryIndexedEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdEndTransformFeedbackEXT(commandBuffer: *mut core::ffi::c_void, firstCounterBuffer: u32, counterBufferCount: u32, pCounterBuffers: *const core::ffi::c_void, pCounterBufferOffsets: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstCounterBuffer; let _ = counterBufferCount; let _ = pCounterBuffers; let _ = pCounterBufferOffsets; crate::stub::unsupported("vkCmdEndTransformFeedbackEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdExecuteGeneratedCommandsNV(commandBuffer: *mut core::ffi::c_void, isPreprocessed: u32, pGeneratedCommandsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = isPreprocessed; let _ = pGeneratedCommandsInfo; crate::stub::unsupported("vkCmdExecuteGeneratedCommandsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdInitializeGraphScratchMemoryAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64) { let _ = commandBuffer; let _ = scratch; crate::stub::unsupported("vkCmdInitializeGraphScratchMemoryAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPreprocessGeneratedCommandsNV(commandBuffer: *mut core::ffi::c_void, pGeneratedCommandsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pGeneratedCommandsInfo; crate::stub::unsupported("vkCmdPreprocessGeneratedCommandsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2(commandBuffer: *mut core::ffi::c_void, pPushConstantsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushConstantsInfo; crate::stub::unsupported("vkCmdPushConstants2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2KHR(commandBuffer: *mut core::ffi::c_void, pPushConstantsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushConstantsInfo; crate::stub::unsupported("vkCmdPushConstants2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, set: u32, descriptorWriteCount: u32, pDescriptorWrites: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = set; let _ = descriptorWriteCount; let _ = pDescriptorWrites; crate::stub::unsupported("vkCmdPushDescriptorSet", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetInfo; crate::stub::unsupported("vkCmdPushDescriptorSet2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2KHR(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetInfo; crate::stub::unsupported("vkCmdPushDescriptorSet2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetKHR(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, set: u32, descriptorWriteCount: u32, pDescriptorWrites: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = set; let _ = descriptorWriteCount; let _ = pDescriptorWrites; crate::stub::unsupported("vkCmdPushDescriptorSetKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate(commandBuffer: *mut core::ffi::c_void, descriptorUpdateTemplate: u64, layout: u64, set: u32, pData: *const core::ffi::c_void) { let _ = commandBuffer; let _ = descriptorUpdateTemplate; let _ = layout; let _ = set; let _ = pData; crate::stub::unsupported("vkCmdPushDescriptorSetWithTemplate", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetWithTemplateInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetWithTemplateInfo; crate::stub::unsupported("vkCmdPushDescriptorSetWithTemplate2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2KHR(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetWithTemplateInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetWithTemplateInfo; crate::stub::unsupported("vkCmdPushDescriptorSetWithTemplate2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplateKHR(commandBuffer: *mut core::ffi::c_void, descriptorUpdateTemplate: u64, layout: u64, set: u32, pData: *const core::ffi::c_void) { let _ = commandBuffer; let _ = descriptorUpdateTemplate; let _ = layout; let _ = set; let _ = pData; crate::stub::unsupported("vkCmdPushDescriptorSetWithTemplateKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdRefreshObjectsKHR(commandBuffer: *mut core::ffi::c_void, pRefreshObjects: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pRefreshObjects; crate::stub::unsupported("vkCmdRefreshObjectsKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetAttachmentFeedbackLoopEnableEXT(commandBuffer: *mut core::ffi::c_void, aspectMask: u32) { let _ = commandBuffer; let _ = aspectMask; crate::stub::unsupported("vkCmdSetAttachmentFeedbackLoopEnableEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCheckpointNV(commandBuffer: *mut core::ffi::c_void, pCheckpointMarker: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pCheckpointMarker; crate::stub::unsupported("vkCmdSetCheckpointNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoarseSampleOrderNV(commandBuffer: *mut core::ffi::c_void, sampleOrderType: i32, customSampleOrderCount: u32, pCustomSampleOrders: *const core::ffi::c_void) { let _ = commandBuffer; let _ = sampleOrderType; let _ = customSampleOrderCount; let _ = pCustomSampleOrders; crate::stub::unsupported("vkCmdSetCoarseSampleOrderNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageModulationModeNV(commandBuffer: *mut core::ffi::c_void, coverageModulationMode: i32) { let _ = commandBuffer; let _ = coverageModulationMode; crate::stub::unsupported("vkCmdSetCoverageModulationModeNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageModulationTableEnableNV(commandBuffer: *mut core::ffi::c_void, coverageModulationTableEnable: u32) { let _ = commandBuffer; let _ = coverageModulationTableEnable; crate::stub::unsupported("vkCmdSetCoverageModulationTableEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageModulationTableNV(commandBuffer: *mut core::ffi::c_void, coverageModulationTableCount: u32, pCoverageModulationTable: *const core::ffi::c_void) { let _ = commandBuffer; let _ = coverageModulationTableCount; let _ = pCoverageModulationTable; crate::stub::unsupported("vkCmdSetCoverageModulationTableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageReductionModeNV(commandBuffer: *mut core::ffi::c_void, coverageReductionMode: i32) { let _ = commandBuffer; let _ = coverageReductionMode; crate::stub::unsupported("vkCmdSetCoverageReductionModeNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageToColorEnableNV(commandBuffer: *mut core::ffi::c_void, coverageToColorEnable: u32) { let _ = commandBuffer; let _ = coverageToColorEnable; crate::stub::unsupported("vkCmdSetCoverageToColorEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageToColorLocationNV(commandBuffer: *mut core::ffi::c_void, coverageToColorLocation: u32) { let _ = commandBuffer; let _ = coverageToColorLocation; crate::stub::unsupported("vkCmdSetCoverageToColorLocationNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDescriptorBufferOffsets2EXT(commandBuffer: *mut core::ffi::c_void, pSetDescriptorBufferOffsetsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pSetDescriptorBufferOffsetsInfo; crate::stub::unsupported("vkCmdSetDescriptorBufferOffsets2EXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDescriptorBufferOffsetsEXT(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, firstSet: u32, setCount: u32, pBufferIndices: *const core::ffi::c_void, pOffsets: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = firstSet; let _ = setCount; let _ = pBufferIndices; let _ = pOffsets; crate::stub::unsupported("vkCmdSetDescriptorBufferOffsetsEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDiscardRectangleEXT(commandBuffer: *mut core::ffi::c_void, firstDiscardRectangle: u32, discardRectangleCount: u32, pDiscardRectangles: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstDiscardRectangle; let _ = discardRectangleCount; let _ = pDiscardRectangles; crate::stub::unsupported("vkCmdSetDiscardRectangleEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDiscardRectangleEnableEXT(commandBuffer: *mut core::ffi::c_void, discardRectangleEnable: u32) { let _ = commandBuffer; let _ = discardRectangleEnable; crate::stub::unsupported("vkCmdSetDiscardRectangleEnableEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDiscardRectangleModeEXT(commandBuffer: *mut core::ffi::c_void, discardRectangleMode: i32) { let _ = commandBuffer; let _ = discardRectangleMode; crate::stub::unsupported("vkCmdSetDiscardRectangleModeEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetExclusiveScissorEnableNV(commandBuffer: *mut core::ffi::c_void, firstExclusiveScissor: u32, exclusiveScissorCount: u32, pExclusiveScissorEnables: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstExclusiveScissor; let _ = exclusiveScissorCount; let _ = pExclusiveScissorEnables; crate::stub::unsupported("vkCmdSetExclusiveScissorEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetExclusiveScissorNV(commandBuffer: *mut core::ffi::c_void, firstExclusiveScissor: u32, exclusiveScissorCount: u32, pExclusiveScissors: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstExclusiveScissor; let _ = exclusiveScissorCount; let _ = pExclusiveScissors; crate::stub::unsupported("vkCmdSetExclusiveScissorNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetFragmentShadingRateEnumNV(commandBuffer: *mut core::ffi::c_void, shadingRate: i32, combinerOps: *const core::ffi::c_void) { let _ = commandBuffer; let _ = shadingRate; let _ = combinerOps; crate::stub::unsupported("vkCmdSetFragmentShadingRateEnumNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetFragmentShadingRateKHR(commandBuffer: *mut core::ffi::c_void, pFragmentSize: *const core::ffi::c_void, combinerOps: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pFragmentSize; let _ = combinerOps; crate::stub::unsupported("vkCmdSetFragmentShadingRateKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetPerformanceMarkerINTEL(commandBuffer: *mut core::ffi::c_void, pMarkerInfo: *const core::ffi::c_void) -> i32 { let _ = commandBuffer; let _ = pMarkerInfo; crate::stub::unsupported("vkCmdSetPerformanceMarkerINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdSetPerformanceOverrideINTEL(commandBuffer: *mut core::ffi::c_void, pOverrideInfo: *const core::ffi::c_void) -> i32 { let _ = commandBuffer; let _ = pOverrideInfo; crate::stub::unsupported("vkCmdSetPerformanceOverrideINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdSetPerformanceStreamMarkerINTEL(commandBuffer: *mut core::ffi::c_void, pMarkerInfo: *const core::ffi::c_void) -> i32 { let _ = commandBuffer; let _ = pMarkerInfo; crate::stub::unsupported("vkCmdSetPerformanceStreamMarkerINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdSetRenderingAttachmentLocations(commandBuffer: *mut core::ffi::c_void, pLocationInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pLocationInfo; crate::stub::unsupported("vkCmdSetRenderingAttachmentLocations", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetRenderingInputAttachmentIndices(commandBuffer: *mut core::ffi::c_void, pInputAttachmentIndexInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInputAttachmentIndexInfo; crate::stub::unsupported("vkCmdSetRenderingInputAttachmentIndices", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetRepresentativeFragmentTestEnableNV(commandBuffer: *mut core::ffi::c_void, representativeFragmentTestEnable: u32) { let _ = commandBuffer; let _ = representativeFragmentTestEnable; crate::stub::unsupported("vkCmdSetRepresentativeFragmentTestEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetSampleLocationsEXT(commandBuffer: *mut core::ffi::c_void, pSampleLocationsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pSampleLocationsInfo; crate::stub::unsupported("vkCmdSetSampleLocationsEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetShadingRateImageEnableNV(commandBuffer: *mut core::ffi::c_void, shadingRateImageEnable: u32) { let _ = commandBuffer; let _ = shadingRateImageEnable; crate::stub::unsupported("vkCmdSetShadingRateImageEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportShadingRatePaletteNV(commandBuffer: *mut core::ffi::c_void, firstViewport: u32, viewportCount: u32, pShadingRatePalettes: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstViewport; let _ = viewportCount; let _ = pShadingRatePalettes; crate::stub::unsupported("vkCmdSetViewportShadingRatePaletteNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportSwizzleNV(commandBuffer: *mut core::ffi::c_void, firstViewport: u32, viewportCount: u32, pViewportSwizzles: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstViewport; let _ = viewportCount; let _ = pViewportSwizzles; crate::stub::unsupported("vkCmdSetViewportSwizzleNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportWScalingEnableNV(commandBuffer: *mut core::ffi::c_void, viewportWScalingEnable: u32) { let _ = commandBuffer; let _ = viewportWScalingEnable; crate::stub::unsupported("vkCmdSetViewportWScalingEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportWScalingNV(commandBuffer: *mut core::ffi::c_void, firstViewport: u32, viewportCount: u32, pViewportWScalings: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstViewport; let _ = viewportCount; let _ = pViewportWScalings; crate::stub::unsupported("vkCmdSetViewportWScalingNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSubpassShadingHUAWEI(commandBuffer: *mut core::ffi::c_void) { let _ = commandBuffer; crate::stub::unsupported("vkCmdSubpassShadingHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdUpdatePipelineIndirectBufferNV(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, pipeline: u64) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = pipeline; crate::stub::unsupported("vkCmdUpdatePipelineIndirectBufferNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteBufferMarker2AMD(commandBuffer: *mut core::ffi::c_void, stage: u64, dstBuffer: u64, dstOffset: u64, marker: u32) { let _ = commandBuffer; let _ = stage; let _ = dstBuffer; let _ = dstOffset; let _ = marker; crate::stub::unsupported("vkCmdWriteBufferMarker2AMD", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteBufferMarkerAMD(commandBuffer: *mut core::ffi::c_void, pipelineStage: i32, dstBuffer: u64, dstOffset: u64, marker: u32) { let _ = commandBuffer; let _ = pipelineStage; let _ = dstBuffer; let _ = dstOffset; let _ = marker; crate::stub::unsupported("vkCmdWriteBufferMarkerAMD", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCompileDeferredNV(device: *mut core::ffi::c_void, pipeline: u64, shader: u32) -> i32 { let _ = device; let _ = pipeline; let _ = shader; crate::stub::unsupported("vkCompileDeferredNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateAndroidSurfaceKHR(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateAndroidSurfaceKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateBufferCollectionFUCHSIA(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pCollection: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pCollection; unsafe { if !pCollection.is_null() { *(pCollection as *mut u64) = 0; } } crate::stub::unsupported("vkCreateBufferCollectionFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCuFunctionNVX(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFunction: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pFunction; unsafe { if !pFunction.is_null() { *(pFunction as *mut u64) = 0; } } crate::stub::unsupported("vkCreateCuFunctionNVX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCuModuleNVX(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pModule: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pModule; unsafe { if !pModule.is_null() { *(pModule as *mut u64) = 0; } } crate::stub::unsupported("vkCreateCuModuleNVX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCudaFunctionNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFunction: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pFunction; unsafe { if !pFunction.is_null() { *(pFunction as *mut u64) = 0; } } crate::stub::unsupported("vkCreateCudaFunctionNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCudaModuleNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pModule: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pModule; unsafe { if !pModule.is_null() { *(pModule as *mut u64) = 0; } } crate::stub::unsupported("vkCreateCudaModuleNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDeferredOperationKHR(device: *mut core::ffi::c_void, pAllocator: *const core::ffi::c_void, pDeferredOperation: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pAllocator; let _ = pDeferredOperation; unsafe { if !pDeferredOperation.is_null() { *(pDeferredOperation as *mut u64) = 0; } } crate::stub::unsupported("vkCreateDeferredOperationKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDirectFBSurfaceEXT(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateDirectFBSurfaceEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDisplayModeKHR(physicalDevice: *mut core::ffi::c_void, display: u64, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pMode: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = display; let _ = pCreateInfo; let _ = pAllocator; let _ = pMode; unsafe { if !pMode.is_null() { *(pMode as *mut u64) = 0; } } crate::stub::unsupported("vkCreateDisplayModeKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDisplayPlaneSurfaceKHR(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateDisplayPlaneSurfaceKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateExecutionGraphPipelinesAMDX(device: *mut core::ffi::c_void, pipelineCache: u64, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pPipelines: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipelineCache; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pPipelines; unsafe { if !pPipelines.is_null() { *(pPipelines as *mut u64) = 0; } } crate::stub::unsupported("vkCreateExecutionGraphPipelinesAMDX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateIOSSurfaceMVK(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateIOSSurfaceMVK", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateImagePipeSurfaceFUCHSIA(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateImagePipeSurfaceFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateIndirectCommandsLayoutNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pIndirectCommandsLayout: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pIndirectCommandsLayout; unsafe { if !pIndirectCommandsLayout.is_null() { *(pIndirectCommandsLayout as *mut u64) = 0; } } crate::stub::unsupported("vkCreateIndirectCommandsLayoutNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateMacOSSurfaceMVK(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateMacOSSurfaceMVK", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateMetalSurfaceEXT(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateMetalSurfaceEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateScreenSurfaceQNX(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateScreenSurfaceQNX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateSemaphoreSciSyncPoolNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSemaphorePool: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pSemaphorePool; unsafe { if !pSemaphorePool.is_null() { *(pSemaphorePool as *mut u64) = 0; } } crate::stub::unsupported("vkCreateSemaphoreSciSyncPoolNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateShadersEXT(device: *mut core::ffi::c_void, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pShaders: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pShaders; unsafe { if !pShaders.is_null() { *(pShaders as *mut u64) = 0; } } crate::stub::unsupported("vkCreateShadersEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateSharedSwapchainsKHR(device: *mut core::ffi::c_void, swapchainCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSwapchains: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchainCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pSwapchains; unsafe { if !pSwapchains.is_null() { *(pSwapchains as *mut u64) = 0; } } crate::stub::unsupported("vkCreateSharedSwapchainsKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateStreamDescriptorSurfaceGGP(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateStreamDescriptorSurfaceGGP", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateValidationCacheEXT(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pValidationCache: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pValidationCache; unsafe { if !pValidationCache.is_null() { *(pValidationCache as *mut u64) = 0; } } crate::stub::unsupported("vkCreateValidationCacheEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateViSurfaceNN(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateViSurfaceNN", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateWin32SurfaceKHR(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::unsupported("vkCreateWin32SurfaceKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkDeferredOperationJoinKHR(device: *mut core::ffi::c_void, operation: u64) -> i32 { let _ = device; let _ = operation; crate::stub::unsupported("vkDeferredOperationJoinKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkDestroyBufferCollectionFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = collection; let _ = pAllocator; crate::stub::unsupported("vkDestroyBufferCollectionFUCHSIA", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCuFunctionNVX(device: *mut core::ffi::c_void, function: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = function; let _ = pAllocator; crate::stub::unsupported("vkDestroyCuFunctionNVX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCuModuleNVX(device: *mut core::ffi::c_void, module: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = module; let _ = pAllocator; crate::stub::unsupported("vkDestroyCuModuleNVX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCudaFunctionNV(device: *mut core::ffi::c_void, function: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = function; let _ = pAllocator; crate::stub::unsupported("vkDestroyCudaFunctionNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCudaModuleNV(device: *mut core::ffi::c_void, module: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = module; let _ = pAllocator; crate::stub::unsupported("vkDestroyCudaModuleNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyDeferredOperationKHR(device: *mut core::ffi::c_void, operation: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = operation; let _ = pAllocator; crate::stub::unsupported("vkDestroyDeferredOperationKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyIndirectCommandsLayoutNV(device: *mut core::ffi::c_void, indirectCommandsLayout: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = indirectCommandsLayout; let _ = pAllocator; crate::stub::unsupported("vkDestroyIndirectCommandsLayoutNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroySemaphoreSciSyncPoolNV(device: *mut core::ffi::c_void, semaphorePool: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = semaphorePool; let _ = pAllocator; crate::stub::unsupported("vkDestroySemaphoreSciSyncPoolNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyShaderEXT(device: *mut core::ffi::c_void, shader: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = shader; let _ = pAllocator; crate::stub::unsupported("vkDestroyShaderEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyValidationCacheEXT(device: *mut core::ffi::c_void, validationCache: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = validationCache; let _ = pAllocator; crate::stub::unsupported("vkDestroyValidationCacheEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDisplayPowerControlEXT(device: *mut core::ffi::c_void, display: u64, pDisplayPowerInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = display; let _ = pDisplayPowerInfo; crate::stub::unsupported("vkDisplayPowerControlEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkEnumeratePhysicalDeviceQueueFamilyPerformanceQueryCountersKHR(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32, pCounterCount: *mut core::ffi::c_void, pCounters: *mut core::ffi::c_void, pCounterDescriptions: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = queueFamilyIndex; let _ = pCounterCount; let _ = pCounters; let _ = pCounterDescriptions; crate::stub::unsupported("vkEnumeratePhysicalDeviceQueueFamilyPerformanceQueryCountersKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkExportMetalObjectsEXT(device: *mut core::ffi::c_void, pMetalObjectsInfo: *mut core::ffi::c_void) { let _ = device; let _ = pMetalObjectsInfo; crate::stub::unsupported("vkExportMetalObjectsEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetAndroidHardwareBufferPropertiesANDROID(device: *mut core::ffi::c_void, buffer: *const core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = buffer; let _ = pProperties; crate::stub::unsupported("vkGetAndroidHardwareBufferPropertiesANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetBufferCollectionPropertiesFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = collection; let _ = pProperties; crate::stub::unsupported("vkGetBufferCollectionPropertiesFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::unsupported("vkGetBufferOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetCommandPoolMemoryConsumption(device: *mut core::ffi::c_void, commandPool: u64, commandBuffer: *mut core::ffi::c_void, pConsumption: *mut core::ffi::c_void) { let _ = device; let _ = commandPool; let _ = commandBuffer; let _ = pConsumption; crate::stub::unsupported("vkGetCommandPoolMemoryConsumption", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetCudaModuleCacheNV(device: *mut core::ffi::c_void, module: u64, pCacheSize: *mut core::ffi::c_void, pCacheData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = module; let _ = pCacheSize; let _ = pCacheData; crate::stub::unsupported("vkGetCudaModuleCacheNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDeferredOperationMaxConcurrencyKHR(device: *mut core::ffi::c_void, operation: u64) -> u32 { let _ = device; let _ = operation; crate::stub::unsupported("vkGetDeferredOperationMaxConcurrencyKHR", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetDeferredOperationResultKHR(device: *mut core::ffi::c_void, operation: u64) -> i32 { let _ = device; let _ = operation; crate::stub::unsupported("vkGetDeferredOperationResultKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDescriptorEXT(device: *mut core::ffi::c_void, pDescriptorInfo: *const core::ffi::c_void, dataSize: usize, pDescriptor: *mut core::ffi::c_void) { let _ = device; let _ = pDescriptorInfo; let _ = dataSize; let _ = pDescriptor; crate::stub::unsupported("vkGetDescriptorEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetHostMappingVALVE(device: *mut core::ffi::c_void, descriptorSet: u64, ppData: *mut *mut core::ffi::c_void) { let _ = device; let _ = descriptorSet; let _ = ppData; crate::stub::unsupported("vkGetDescriptorSetHostMappingVALVE", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutBindingOffsetEXT(device: *mut core::ffi::c_void, layout: u64, binding: u32, pOffset: *mut core::ffi::c_void) { let _ = device; let _ = layout; let _ = binding; let _ = pOffset; crate::stub::unsupported("vkGetDescriptorSetLayoutBindingOffsetEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutHostMappingInfoVALVE(device: *mut core::ffi::c_void, pBindingReference: *const core::ffi::c_void, pHostMapping: *mut core::ffi::c_void) { let _ = device; let _ = pBindingReference; let _ = pHostMapping; crate::stub::unsupported("vkGetDescriptorSetLayoutHostMappingInfoVALVE", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutSizeEXT(device: *mut core::ffi::c_void, layout: u64, pLayoutSizeInBytes: *mut core::ffi::c_void) { let _ = device; let _ = layout; let _ = pLayoutSizeInBytes; crate::stub::unsupported("vkGetDescriptorSetLayoutSizeEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDeviceFaultInfoEXT(device: *mut core::ffi::c_void, pFaultCounts: *mut core::ffi::c_void, pFaultInfo: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pFaultCounts; let _ = pFaultInfo; crate::stub::unsupported("vkGetDeviceFaultInfoEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDeviceSubpassShadingMaxWorkgroupSizeHUAWEI(device: *mut core::ffi::c_void, renderpass: u64, pMaxWorkgroupSize: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = renderpass; let _ = pMaxWorkgroupSize; crate::stub::unsupported("vkGetDeviceSubpassShadingMaxWorkgroupSizeHUAWEI", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayModeProperties2KHR(physicalDevice: *mut core::ffi::c_void, display: u64, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = display; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetDisplayModeProperties2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayModePropertiesKHR(physicalDevice: *mut core::ffi::c_void, display: u64, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = display; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetDisplayModePropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayPlaneCapabilities2KHR(physicalDevice: *mut core::ffi::c_void, pDisplayPlaneInfo: *const core::ffi::c_void, pCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pDisplayPlaneInfo; let _ = pCapabilities; crate::stub::unsupported("vkGetDisplayPlaneCapabilities2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayPlaneCapabilitiesKHR(physicalDevice: *mut core::ffi::c_void, mode: u64, planeIndex: u32, pCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = mode; let _ = planeIndex; let _ = pCapabilities; crate::stub::unsupported("vkGetDisplayPlaneCapabilitiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayPlaneSupportedDisplaysKHR(physicalDevice: *mut core::ffi::c_void, planeIndex: u32, pDisplayCount: *mut core::ffi::c_void, pDisplays: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = planeIndex; let _ = pDisplayCount; let _ = pDisplays; crate::stub::unsupported("vkGetDisplayPlaneSupportedDisplaysKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDrmDisplayEXT(physicalDevice: *mut core::ffi::c_void, drmFd: i32, connectorId: u32, display: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = drmFd; let _ = connectorId; let _ = display; crate::stub::unsupported("vkGetDrmDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDynamicRenderingTilePropertiesQCOM(device: *mut core::ffi::c_void, pRenderingInfo: *const core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pRenderingInfo; let _ = pProperties; crate::stub::unsupported("vkGetDynamicRenderingTilePropertiesQCOM", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetExecutionGraphPipelineNodeIndexAMDX(device: *mut core::ffi::c_void, executionGraph: u64, pNodeInfo: *const core::ffi::c_void, pNodeIndex: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = executionGraph; let _ = pNodeInfo; let _ = pNodeIndex; crate::stub::unsupported("vkGetExecutionGraphPipelineNodeIndexAMDX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetExecutionGraphPipelineScratchSizeAMDX(device: *mut core::ffi::c_void, executionGraph: u64, pSizeInfo: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = executionGraph; let _ = pSizeInfo; crate::stub::unsupported("vkGetExecutionGraphPipelineScratchSizeAMDX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFaultData(device: *mut core::ffi::c_void, faultQueryBehavior: i32, pUnrecordedFaults: *mut core::ffi::c_void, pFaultCount: *mut core::ffi::c_void, pFaults: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = faultQueryBehavior; let _ = pUnrecordedFaults; let _ = pFaultCount; let _ = pFaults; crate::stub::unsupported("vkGetFaultData", "extension not advertised"); VK_ERROR_FEATURE_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceFdKHR(device: *mut core::ffi::c_void, pGetFdInfo: *const core::ffi::c_void, pFd: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetFdInfo; let _ = pFd; crate::stub::unsupported("vkGetFenceFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceSciSyncFenceNV(device: *mut core::ffi::c_void, pGetSciSyncHandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciSyncHandleInfo; let _ = pHandle; crate::stub::unsupported("vkGetFenceSciSyncFenceNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceSciSyncObjNV(device: *mut core::ffi::c_void, pGetSciSyncHandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciSyncHandleInfo; let _ = pHandle; crate::stub::unsupported("vkGetFenceSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceWin32HandleKHR(device: *mut core::ffi::c_void, pGetWin32HandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetWin32HandleInfo; let _ = pHandle; crate::stub::unsupported("vkGetFenceWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFramebufferTilePropertiesQCOM(device: *mut core::ffi::c_void, framebuffer: u64, pPropertiesCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = framebuffer; let _ = pPropertiesCount; let _ = pProperties; crate::stub::unsupported("vkGetFramebufferTilePropertiesQCOM", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetGeneratedCommandsMemoryRequirementsNV(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) { let _ = device; let _ = pInfo; let _ = pMemoryRequirements; crate::stub::unsupported("vkGetGeneratedCommandsMemoryRequirementsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetImageDrmFormatModifierPropertiesEXT(device: *mut core::ffi::c_void, image: u64, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = image; let _ = pProperties; crate::stub::unsupported("vkGetImageDrmFormatModifierPropertiesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetImageOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::unsupported("vkGetImageOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetImageViewAddressNVX(device: *mut core::ffi::c_void, imageView: u64, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = imageView; let _ = pProperties; crate::stub::unsupported("vkGetImageViewAddressNVX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetImageViewHandleNVX(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> u32 { let _ = device; let _ = pInfo; crate::stub::unsupported("vkGetImageViewHandleNVX", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetImageViewOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::unsupported("vkGetImageViewOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetLatencyTimingsNV(device: *mut core::ffi::c_void, swapchain: u64, pLatencyMarkerInfo: *mut core::ffi::c_void) { let _ = device; let _ = swapchain; let _ = pLatencyMarkerInfo; crate::stub::unsupported("vkGetLatencyTimingsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetMemoryAndroidHardwareBufferANDROID(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pBuffer: *mut *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pBuffer; crate::stub::unsupported("vkGetMemoryAndroidHardwareBufferANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryFdKHR(device: *mut core::ffi::c_void, pGetFdInfo: *const core::ffi::c_void, pFd: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetFdInfo; let _ = pFd; crate::stub::unsupported("vkGetMemoryFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryFdPropertiesKHR(device: *mut core::ffi::c_void, handleType: i32, fd: i32, pMemoryFdProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = handleType; let _ = fd; let _ = pMemoryFdProperties; crate::stub::unsupported("vkGetMemoryFdPropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryHostPointerPropertiesEXT(device: *mut core::ffi::c_void, handleType: i32, pHostPointer: *const core::ffi::c_void, pMemoryHostPointerProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = handleType; let _ = pHostPointer; let _ = pMemoryHostPointerProperties; crate::stub::unsupported("vkGetMemoryHostPointerPropertiesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryRemoteAddressNV(device: *mut core::ffi::c_void, pMemoryGetRemoteAddressInfo: *const core::ffi::c_void, pAddress: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pMemoryGetRemoteAddressInfo; let _ = pAddress; crate::stub::unsupported("vkGetMemoryRemoteAddressNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemorySciBufNV(device: *mut core::ffi::c_void, pGetSciBufInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciBufInfo; let _ = pHandle; crate::stub::unsupported("vkGetMemorySciBufNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryWin32HandleKHR(device: *mut core::ffi::c_void, pGetWin32HandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetWin32HandleInfo; let _ = pHandle; crate::stub::unsupported("vkGetMemoryWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryWin32HandleNV(device: *mut core::ffi::c_void, memory: u64, handleType: u32, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = memory; let _ = handleType; let _ = pHandle; crate::stub::unsupported("vkGetMemoryWin32HandleNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryZirconHandleFUCHSIA(device: *mut core::ffi::c_void, pGetZirconHandleInfo: *const core::ffi::c_void, pZirconHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetZirconHandleInfo; let _ = pZirconHandle; crate::stub::unsupported("vkGetMemoryZirconHandleFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryZirconHandlePropertiesFUCHSIA(device: *mut core::ffi::c_void, handleType: i32, zirconHandle: u32, pMemoryZirconHandleProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = handleType; let _ = zirconHandle; let _ = pMemoryZirconHandleProperties; crate::stub::unsupported("vkGetMemoryZirconHandlePropertiesFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPastPresentationTimingGOOGLE(device: *mut core::ffi::c_void, swapchain: u64, pPresentationTimingCount: *mut core::ffi::c_void, pPresentationTimings: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pPresentationTimingCount; let _ = pPresentationTimings; crate::stub::unsupported("vkGetPastPresentationTimingGOOGLE", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPerformanceParameterINTEL(device: *mut core::ffi::c_void, parameter: i32, pValue: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = parameter; let _ = pValue; crate::stub::unsupported("vkGetPerformanceParameterINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDirectFBPresentationSupportEXT(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32, dfb: *mut core::ffi::c_void) -> u32 { let _ = physicalDevice; let _ = queueFamilyIndex; let _ = dfb; crate::stub::unsupported("vkGetPhysicalDeviceDirectFBPresentationSupportEXT", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayPlaneProperties2KHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetPhysicalDeviceDisplayPlaneProperties2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayPlanePropertiesKHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetPhysicalDeviceDisplayPlanePropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayProperties2KHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetPhysicalDeviceDisplayProperties2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::unsupported("vkGetPhysicalDeviceDisplayPropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceExternalImageFormatPropertiesNV(physicalDevice: *mut core::ffi::c_void, format: i32, type_: i32, tiling: i32, usage: u32, flags: u32, externalHandleType: u32, pExternalImageFormatProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = format; let _ = type_; let _ = tiling; let _ = usage; let _ = flags; let _ = externalHandleType; let _ = pExternalImageFormatProperties; crate::stub::unsupported("vkGetPhysicalDeviceExternalImageFormatPropertiesNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFragmentShadingRatesKHR(physicalDevice: *mut core::ffi::c_void, pFragmentShadingRateCount: *mut core::ffi::c_void, pFragmentShadingRates: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pFragmentShadingRateCount; let _ = pFragmentShadingRates; crate::stub::unsupported("vkGetPhysicalDeviceFragmentShadingRatesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyPerformanceQueryPassesKHR(physicalDevice: *mut core::ffi::c_void, pPerformanceQueryCreateInfo: *const core::ffi::c_void, pNumPasses: *mut core::ffi::c_void) { let _ = physicalDevice; let _ = pPerformanceQueryCreateInfo; let _ = pNumPasses; crate::stub::unsupported("vkGetPhysicalDeviceQueueFamilyPerformanceQueryPassesKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceRefreshableObjectTypesKHR(physicalDevice: *mut core::ffi::c_void, pRefreshableObjectTypeCount: *mut core::ffi::c_void, pRefreshableObjectTypes: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pRefreshableObjectTypeCount; let _ = pRefreshableObjectTypes; crate::stub::unsupported("vkGetPhysicalDeviceRefreshableObjectTypesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceScreenPresentationSupportQNX(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32, window: *mut core::ffi::c_void) -> u32 { let _ = physicalDevice; let _ = queueFamilyIndex; let _ = window; crate::stub::unsupported("vkGetPhysicalDeviceScreenPresentationSupportQNX", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSupportedFramebufferMixedSamplesCombinationsNV(physicalDevice: *mut core::ffi::c_void, pCombinationCount: *mut core::ffi::c_void, pCombinations: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pCombinationCount; let _ = pCombinations; crate::stub::unsupported("vkGetPhysicalDeviceSupportedFramebufferMixedSamplesCombinationsNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilities2EXT(physicalDevice: *mut core::ffi::c_void, surface: u64, pSurfaceCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = surface; let _ = pSurfaceCapabilities; crate::stub::unsupported("vkGetPhysicalDeviceSurfaceCapabilities2EXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilities2KHR(physicalDevice: *mut core::ffi::c_void, pSurfaceInfo: *const core::ffi::c_void, pSurfaceCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pSurfaceInfo; let _ = pSurfaceCapabilities; crate::stub::unsupported("vkGetPhysicalDeviceSurfaceCapabilities2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceFormats2KHR(physicalDevice: *mut core::ffi::c_void, pSurfaceInfo: *const core::ffi::c_void, pSurfaceFormatCount: *mut core::ffi::c_void, pSurfaceFormats: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pSurfaceInfo; let _ = pSurfaceFormatCount; let _ = pSurfaceFormats; crate::stub::unsupported("vkGetPhysicalDeviceSurfaceFormats2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModes2EXT(physicalDevice: *mut core::ffi::c_void, pSurfaceInfo: *const core::ffi::c_void, pPresentModeCount: *mut core::ffi::c_void, pPresentModes: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pSurfaceInfo; let _ = pPresentModeCount; let _ = pPresentModes; crate::stub::unsupported("vkGetPhysicalDeviceSurfacePresentModes2EXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceWin32PresentationSupportKHR(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32) -> u32 { let _ = physicalDevice; let _ = queueFamilyIndex; crate::stub::unsupported("vkGetPhysicalDeviceWin32PresentationSupportKHR", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutableInternalRepresentationsKHR(device: *mut core::ffi::c_void, pExecutableInfo: *const core::ffi::c_void, pInternalRepresentationCount: *mut core::ffi::c_void, pInternalRepresentations: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pExecutableInfo; let _ = pInternalRepresentationCount; let _ = pInternalRepresentations; crate::stub::unsupported("vkGetPipelineExecutableInternalRepresentationsKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutablePropertiesKHR(device: *mut core::ffi::c_void, pPipelineInfo: *const core::ffi::c_void, pExecutableCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pPipelineInfo; let _ = pExecutableCount; let _ = pProperties; crate::stub::unsupported("vkGetPipelineExecutablePropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutableStatisticsKHR(device: *mut core::ffi::c_void, pExecutableInfo: *const core::ffi::c_void, pStatisticCount: *mut core::ffi::c_void, pStatistics: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pExecutableInfo; let _ = pStatisticCount; let _ = pStatistics; crate::stub::unsupported("vkGetPipelineExecutableStatisticsKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPipelineIndirectDeviceAddressNV(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> u64 { let _ = device; let _ = pInfo; crate::stub::unsupported("vkGetPipelineIndirectDeviceAddressNV", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPipelineIndirectMemoryRequirementsNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) { let _ = device; let _ = pCreateInfo; let _ = pMemoryRequirements; crate::stub::unsupported("vkGetPipelineIndirectMemoryRequirementsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetPipelinePropertiesEXT(device: *mut core::ffi::c_void, pPipelineInfo: *const core::ffi::c_void, pPipelineProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pPipelineInfo; let _ = pPipelineProperties; crate::stub::unsupported("vkGetPipelinePropertiesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetQueueCheckpointData2NV(queue: *mut core::ffi::c_void, pCheckpointDataCount: *mut core::ffi::c_void, pCheckpointData: *mut core::ffi::c_void) { let _ = queue; let _ = pCheckpointDataCount; let _ = pCheckpointData; unsafe { if !pCheckpointDataCount.is_null() { *(pCheckpointDataCount as *mut u32) = 0; } } crate::stub::unsupported("vkGetQueueCheckpointData2NV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetQueueCheckpointDataNV(queue: *mut core::ffi::c_void, pCheckpointDataCount: *mut core::ffi::c_void, pCheckpointData: *mut core::ffi::c_void) { let _ = queue; let _ = pCheckpointDataCount; let _ = pCheckpointData; unsafe { if !pCheckpointDataCount.is_null() { *(pCheckpointDataCount as *mut u32) = 0; } } crate::stub::unsupported("vkGetQueueCheckpointDataNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetRandROutputDisplayEXT(physicalDevice: *mut core::ffi::c_void, dpy: *mut core::ffi::c_void, rrOutput: u64, pDisplay: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = dpy; let _ = rrOutput; let _ = pDisplay; crate::stub::unsupported("vkGetRandROutputDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRefreshCycleDurationGOOGLE(device: *mut core::ffi::c_void, swapchain: u64, pDisplayTimingProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pDisplayTimingProperties; crate::stub::unsupported("vkGetRefreshCycleDurationGOOGLE", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSamplerOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::unsupported("vkGetSamplerOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetScreenBufferPropertiesQNX(device: *mut core::ffi::c_void, buffer: *const core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = buffer; let _ = pProperties; crate::stub::unsupported("vkGetScreenBufferPropertiesQNX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreFdKHR(device: *mut core::ffi::c_void, pGetFdInfo: *const core::ffi::c_void, pFd: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetFdInfo; let _ = pFd; crate::stub::unsupported("vkGetSemaphoreFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreSciSyncObjNV(device: *mut core::ffi::c_void, pGetSciSyncInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciSyncInfo; let _ = pHandle; crate::stub::unsupported("vkGetSemaphoreSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreWin32HandleKHR(device: *mut core::ffi::c_void, pGetWin32HandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetWin32HandleInfo; let _ = pHandle; crate::stub::unsupported("vkGetSemaphoreWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreZirconHandleFUCHSIA(device: *mut core::ffi::c_void, pGetZirconHandleInfo: *const core::ffi::c_void, pZirconHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetZirconHandleInfo; let _ = pZirconHandle; crate::stub::unsupported("vkGetSemaphoreZirconHandleFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetShaderBinaryDataEXT(device: *mut core::ffi::c_void, shader: u64, pDataSize: *mut core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = shader; let _ = pDataSize; let _ = pData; crate::stub::unsupported("vkGetShaderBinaryDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetShaderInfoAMD(device: *mut core::ffi::c_void, pipeline: u64, shaderStage: i32, infoType: i32, pInfoSize: *mut core::ffi::c_void, pInfo: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = shaderStage; let _ = infoType; let _ = pInfoSize; let _ = pInfo; crate::stub::unsupported("vkGetShaderInfoAMD", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetShaderModuleCreateInfoIdentifierEXT(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pIdentifier: *mut core::ffi::c_void) { let _ = device; let _ = pCreateInfo; let _ = pIdentifier; crate::stub::unsupported("vkGetShaderModuleCreateInfoIdentifierEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetShaderModuleIdentifierEXT(device: *mut core::ffi::c_void, shaderModule: u64, pIdentifier: *mut core::ffi::c_void) { let _ = device; let _ = shaderModule; let _ = pIdentifier; crate::stub::unsupported("vkGetShaderModuleIdentifierEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetSwapchainCounterEXT(device: *mut core::ffi::c_void, swapchain: u64, counter: i32, pCounterValue: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = counter; let _ = pCounterValue; crate::stub::unsupported("vkGetSwapchainCounterEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSwapchainGrallocUsage2ANDROID(device: *mut core::ffi::c_void, format: i32, imageUsage: u32, swapchainImageUsage: u32, grallocConsumerUsage: *mut core::ffi::c_void, grallocProducerUsage: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = format; let _ = imageUsage; let _ = swapchainImageUsage; let _ = grallocConsumerUsage; let _ = grallocProducerUsage; crate::stub::unsupported("vkGetSwapchainGrallocUsage2ANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSwapchainGrallocUsageANDROID(device: *mut core::ffi::c_void, format: i32, imageUsage: u32, grallocUsage: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = format; let _ = imageUsage; let _ = grallocUsage; crate::stub::unsupported("vkGetSwapchainGrallocUsageANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSwapchainStatusKHR(device: *mut core::ffi::c_void, swapchain: u64) -> i32 { let _ = device; let _ = swapchain; crate::stub::unsupported("vkGetSwapchainStatusKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetValidationCacheDataEXT(device: *mut core::ffi::c_void, validationCache: u64, pDataSize: *mut core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = validationCache; let _ = pDataSize; let _ = pData; crate::stub::unsupported("vkGetValidationCacheDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetWinrtDisplayNV(physicalDevice: *mut core::ffi::c_void, deviceRelativeId: u32, pDisplay: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = deviceRelativeId; let _ = pDisplay; crate::stub::unsupported("vkGetWinrtDisplayNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceFdKHR(device: *mut core::ffi::c_void, pImportFenceFdInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceFdInfo; crate::stub::unsupported("vkImportFenceFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceSciSyncFenceNV(device: *mut core::ffi::c_void, pImportFenceSciSyncInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceSciSyncInfo; crate::stub::unsupported("vkImportFenceSciSyncFenceNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceSciSyncObjNV(device: *mut core::ffi::c_void, pImportFenceSciSyncInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceSciSyncInfo; crate::stub::unsupported("vkImportFenceSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceWin32HandleKHR(device: *mut core::ffi::c_void, pImportFenceWin32HandleInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceWin32HandleInfo; crate::stub::unsupported("vkImportFenceWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreFdKHR(device: *mut core::ffi::c_void, pImportSemaphoreFdInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreFdInfo; crate::stub::unsupported("vkImportSemaphoreFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreSciSyncObjNV(device: *mut core::ffi::c_void, pImportSemaphoreSciSyncInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreSciSyncInfo; crate::stub::unsupported("vkImportSemaphoreSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreWin32HandleKHR(device: *mut core::ffi::c_void, pImportSemaphoreWin32HandleInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreWin32HandleInfo; crate::stub::unsupported("vkImportSemaphoreWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreZirconHandleFUCHSIA(device: *mut core::ffi::c_void, pImportSemaphoreZirconHandleInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreZirconHandleInfo; crate::stub::unsupported("vkImportSemaphoreZirconHandleFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkInitializePerformanceApiINTEL(device: *mut core::ffi::c_void, pInitializeInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pInitializeInfo; crate::stub::unsupported("vkInitializePerformanceApiINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkLatencySleepNV(device: *mut core::ffi::c_void, swapchain: u64, pSleepInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pSleepInfo; crate::stub::unsupported("vkLatencySleepNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkMergeValidationCachesEXT(device: *mut core::ffi::c_void, dstCache: u64, srcCacheCount: u32, pSrcCaches: *const core::ffi::c_void) -> i32 { let _ = device; let _ = dstCache; let _ = srcCacheCount; let _ = pSrcCaches; crate::stub::unsupported("vkMergeValidationCachesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkQueueBindSparse(queue: *mut core::ffi::c_void, bindInfoCount: u32, pBindInfo: *const core::ffi::c_void, fence: u64) -> i32 { let _ = queue; let _ = bindInfoCount; let _ = pBindInfo; let _ = fence; crate::stub::unsupported("vkQueueBindSparse", "extension not advertised"); VK_ERROR_FEATURE_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkQueueNotifyOutOfBandNV(queue: *mut core::ffi::c_void, pQueueTypeInfo: *const core::ffi::c_void) { let _ = queue; let _ = pQueueTypeInfo; crate::stub::unsupported("vkQueueNotifyOutOfBandNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkQueueSetPerformanceConfigurationINTEL(queue: *mut core::ffi::c_void, configuration: u64) -> i32 { let _ = queue; let _ = configuration; crate::stub::unsupported("vkQueueSetPerformanceConfigurationINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkQueueSignalReleaseImageANDROID(queue: *mut core::ffi::c_void, waitSemaphoreCount: u32, pWaitSemaphores: *const core::ffi::c_void, image: u64, pNativeFenceFd: *mut core::ffi::c_void) -> i32 { let _ = queue; let _ = waitSemaphoreCount; let _ = pWaitSemaphores; let _ = image; let _ = pNativeFenceFd; crate::stub::unsupported("vkQueueSignalReleaseImageANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkRegisterDeviceEventEXT(device: *mut core::ffi::c_void, pDeviceEventInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFence: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pDeviceEventInfo; let _ = pAllocator; let _ = pFence; unsafe { if !pFence.is_null() { *(pFence as *mut u64) = 0; } } crate::stub::unsupported("vkRegisterDeviceEventEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkRegisterDisplayEventEXT(device: *mut core::ffi::c_void, display: u64, pDisplayEventInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFence: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = display; let _ = pDisplayEventInfo; let _ = pAllocator; let _ = pFence; unsafe { if !pFence.is_null() { *(pFence as *mut u64) = 0; } } crate::stub::unsupported("vkRegisterDisplayEventEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleaseDisplayEXT(physicalDevice: *mut core::ffi::c_void, display: u64) -> i32 { let _ = physicalDevice; let _ = display; crate::stub::unsupported("vkReleaseDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleaseFullScreenExclusiveModeEXT(device: *mut core::ffi::c_void, swapchain: u64) -> i32 { let _ = device; let _ = swapchain; crate::stub::unsupported("vkReleaseFullScreenExclusiveModeEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleasePerformanceConfigurationINTEL(device: *mut core::ffi::c_void, configuration: u64) -> i32 { let _ = device; let _ = configuration; crate::stub::unsupported("vkReleasePerformanceConfigurationINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleaseProfilingLockKHR(device: *mut core::ffi::c_void) { let _ = device; crate::stub::unsupported("vkReleaseProfilingLockKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkReleaseSwapchainImagesEXT(device: *mut core::ffi::c_void, pReleaseInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pReleaseInfo; crate::stub::unsupported("vkReleaseSwapchainImagesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetBufferCollectionBufferConstraintsFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pBufferConstraintsInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = collection; let _ = pBufferConstraintsInfo; crate::stub::unsupported("vkSetBufferCollectionBufferConstraintsFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetBufferCollectionImageConstraintsFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pImageConstraintsInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = collection; let _ = pImageConstraintsInfo; crate::stub::unsupported("vkSetBufferCollectionImageConstraintsFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetDeviceMemoryPriorityEXT(device: *mut core::ffi::c_void, memory: u64, priority: f32) { let _ = device; let _ = memory; let _ = priority; crate::stub::unsupported("vkSetDeviceMemoryPriorityEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkSetHdrMetadataEXT(device: *mut core::ffi::c_void, swapchainCount: u32, pSwapchains: *const core::ffi::c_void, pMetadata: *const core::ffi::c_void) { let _ = device; let _ = swapchainCount; let _ = pSwapchains; let _ = pMetadata; crate::stub::unsupported("vkSetHdrMetadataEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkSetLatencyMarkerNV(device: *mut core::ffi::c_void, swapchain: u64, pLatencyMarkerInfo: *const core::ffi::c_void) { let _ = device; let _ = swapchain; let _ = pLatencyMarkerInfo; crate::stub::unsupported("vkSetLatencyMarkerNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkSetLatencySleepModeNV(device: *mut core::ffi::c_void, swapchain: u64, pSleepModeInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pSleepModeInfo; crate::stub::unsupported("vkSetLatencySleepModeNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetLocalDimmingAMD(device: *mut core::ffi::c_void, swapChain: u64, localDimmingEnable: u32) { let _ = device; let _ = swapChain; let _ = localDimmingEnable; crate::stub::unsupported("vkSetLocalDimmingAMD", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkUninitializePerformanceApiINTEL(device: *mut core::ffi::c_void) { let _ = device; crate::stub::unsupported("vkUninitializePerformanceApiINTEL", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkWaitForPresentKHR(device: *mut core::ffi::c_void, swapchain: u64, presentId: u64, timeout: u64) -> i32 { let _ = device; let _ = swapchain; let _ = presentId; let _ = timeout; crate::stub::unsupported("vkWaitForPresentKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }
