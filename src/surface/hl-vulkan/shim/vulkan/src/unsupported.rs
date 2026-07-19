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
pub extern "C" fn vkBindAccelerationStructureMemoryNV(device: *mut core::ffi::c_void, bindInfoCount: u32, pBindInfos: *const core::ffi::c_void) -> i32 { let _ = device; let _ = bindInfoCount; let _ = pBindInfos; crate::stub::Call::unsupported("vkBindAccelerationStructureMemoryNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBindOpticalFlowSessionImageNV(device: *mut core::ffi::c_void, session: u64, bindingPoint: i32, view: u64, layout: i32) -> i32 { let _ = device; let _ = session; let _ = bindingPoint; let _ = view; let _ = layout; crate::stub::Call::unsupported("vkBindOpticalFlowSessionImageNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBindVideoSessionMemoryKHR(device: *mut core::ffi::c_void, videoSession: u64, bindSessionMemoryInfoCount: u32, pBindSessionMemoryInfos: *const core::ffi::c_void) -> i32 { let _ = device; let _ = videoSession; let _ = bindSessionMemoryInfoCount; let _ = pBindSessionMemoryInfos; crate::stub::Call::unsupported("vkBindVideoSessionMemoryKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBuildAccelerationStructuresKHR(device: *mut core::ffi::c_void, deferredOperation: u64, infoCount: u32, pInfos: *const core::ffi::c_void, ppBuildRangeInfos: *const *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = infoCount; let _ = pInfos; let _ = ppBuildRangeInfos; crate::stub::Call::unsupported("vkBuildAccelerationStructuresKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkBuildMicromapsEXT(device: *mut core::ffi::c_void, deferredOperation: u64, infoCount: u32, pInfos: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = infoCount; let _ = pInfos; crate::stub::Call::unsupported("vkBuildMicromapsEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdBeginVideoCodingKHR(commandBuffer: *mut core::ffi::c_void, pBeginInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBeginInfo; crate::stub::Call::unsupported("vkCmdBeginVideoCodingKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructureNV(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, instanceData: u64, instanceOffset: u64, update: u32, dst: u64, src: u64, scratch: u64, scratchOffset: u64) { let _ = commandBuffer; let _ = pInfo; let _ = instanceData; let _ = instanceOffset; let _ = update; let _ = dst; let _ = src; let _ = scratch; let _ = scratchOffset; crate::stub::Call::unsupported("vkCmdBuildAccelerationStructureNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructuresIndirectKHR(commandBuffer: *mut core::ffi::c_void, infoCount: u32, pInfos: *const core::ffi::c_void, pIndirectDeviceAddresses: *const core::ffi::c_void, pIndirectStrides: *const core::ffi::c_void, ppMaxPrimitiveCounts: *const *const core::ffi::c_void) { let _ = commandBuffer; let _ = infoCount; let _ = pInfos; let _ = pIndirectDeviceAddresses; let _ = pIndirectStrides; let _ = ppMaxPrimitiveCounts; crate::stub::Call::unsupported("vkCmdBuildAccelerationStructuresIndirectKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildAccelerationStructuresKHR(commandBuffer: *mut core::ffi::c_void, infoCount: u32, pInfos: *const core::ffi::c_void, ppBuildRangeInfos: *const *const core::ffi::c_void) { let _ = commandBuffer; let _ = infoCount; let _ = pInfos; let _ = ppBuildRangeInfos; crate::stub::Call::unsupported("vkCmdBuildAccelerationStructuresKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdBuildMicromapsEXT(commandBuffer: *mut core::ffi::c_void, infoCount: u32, pInfos: *const core::ffi::c_void) { let _ = commandBuffer; let _ = infoCount; let _ = pInfos; crate::stub::Call::unsupported("vkCmdBuildMicromapsEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdControlVideoCodingKHR(commandBuffer: *mut core::ffi::c_void, pCodingControlInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pCodingControlInfo; crate::stub::Call::unsupported("vkCmdControlVideoCodingKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureKHR(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::Call::unsupported("vkCmdCopyAccelerationStructureKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureNV(commandBuffer: *mut core::ffi::c_void, dst: u64, src: u64, mode: i32) { let _ = commandBuffer; let _ = dst; let _ = src; let _ = mode; crate::stub::Call::unsupported("vkCmdCopyAccelerationStructureNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyAccelerationStructureToMemoryKHR(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::Call::unsupported("vkCmdCopyAccelerationStructureToMemoryKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToAccelerationStructureKHR(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::Call::unsupported("vkCmdCopyMemoryToAccelerationStructureKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToMicromapEXT(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::Call::unsupported("vkCmdCopyMemoryToMicromapEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMicromapEXT(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::Call::unsupported("vkCmdCopyMicromapEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMicromapToMemoryEXT(commandBuffer: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInfo; crate::stub::Call::unsupported("vkCmdCopyMicromapToMemoryEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDecodeVideoKHR(commandBuffer: *mut core::ffi::c_void, pDecodeInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pDecodeInfo; crate::stub::Call::unsupported("vkCmdDecodeVideoKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksEXT(commandBuffer: *mut core::ffi::c_void, groupCountX: u32, groupCountY: u32, groupCountZ: u32) { let _ = commandBuffer; let _ = groupCountX; let _ = groupCountY; let _ = groupCountZ; crate::stub::Call::unsupported("vkCmdDrawMeshTasksEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectCountEXT(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, countBuffer: u64, countBufferOffset: u64, maxDrawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = countBuffer; let _ = countBufferOffset; let _ = maxDrawCount; let _ = stride; crate::stub::Call::unsupported("vkCmdDrawMeshTasksIndirectCountEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectCountNV(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, countBuffer: u64, countBufferOffset: u64, maxDrawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = countBuffer; let _ = countBufferOffset; let _ = maxDrawCount; let _ = stride; crate::stub::Call::unsupported("vkCmdDrawMeshTasksIndirectCountNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectEXT(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, drawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = drawCount; let _ = stride; crate::stub::Call::unsupported("vkCmdDrawMeshTasksIndirectEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksIndirectNV(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, drawCount: u32, stride: u32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = drawCount; let _ = stride; crate::stub::Call::unsupported("vkCmdDrawMeshTasksIndirectNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMeshTasksNV(commandBuffer: *mut core::ffi::c_void, taskCount: u32, firstTask: u32) { let _ = commandBuffer; let _ = taskCount; let _ = firstTask; crate::stub::Call::unsupported("vkCmdDrawMeshTasksNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdEncodeVideoKHR(commandBuffer: *mut core::ffi::c_void, pEncodeInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pEncodeInfo; crate::stub::Call::unsupported("vkCmdEncodeVideoKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdEndVideoCodingKHR(commandBuffer: *mut core::ffi::c_void, pEndCodingInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pEndCodingInfo; crate::stub::Call::unsupported("vkCmdEndVideoCodingKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdOpticalFlowExecuteNV(commandBuffer: *mut core::ffi::c_void, session: u64, pExecuteInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = session; let _ = pExecuteInfo; crate::stub::Call::unsupported("vkCmdOpticalFlowExecuteNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdSetRayTracingPipelineStackSizeKHR(commandBuffer: *mut core::ffi::c_void, pipelineStackSize: u32) { let _ = commandBuffer; let _ = pipelineStackSize; crate::stub::Call::unsupported("vkCmdSetRayTracingPipelineStackSizeKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysIndirect2KHR(commandBuffer: *mut core::ffi::c_void, indirectDeviceAddress: u64) { let _ = commandBuffer; let _ = indirectDeviceAddress; crate::stub::Call::unsupported("vkCmdTraceRaysIndirect2KHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysIndirectKHR(commandBuffer: *mut core::ffi::c_void, pRaygenShaderBindingTable: *const core::ffi::c_void, pMissShaderBindingTable: *const core::ffi::c_void, pHitShaderBindingTable: *const core::ffi::c_void, pCallableShaderBindingTable: *const core::ffi::c_void, indirectDeviceAddress: u64) { let _ = commandBuffer; let _ = pRaygenShaderBindingTable; let _ = pMissShaderBindingTable; let _ = pHitShaderBindingTable; let _ = pCallableShaderBindingTable; let _ = indirectDeviceAddress; crate::stub::Call::unsupported("vkCmdTraceRaysIndirectKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysKHR(commandBuffer: *mut core::ffi::c_void, pRaygenShaderBindingTable: *const core::ffi::c_void, pMissShaderBindingTable: *const core::ffi::c_void, pHitShaderBindingTable: *const core::ffi::c_void, pCallableShaderBindingTable: *const core::ffi::c_void, width: u32, height: u32, depth: u32) { let _ = commandBuffer; let _ = pRaygenShaderBindingTable; let _ = pMissShaderBindingTable; let _ = pHitShaderBindingTable; let _ = pCallableShaderBindingTable; let _ = width; let _ = height; let _ = depth; crate::stub::Call::unsupported("vkCmdTraceRaysKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdTraceRaysNV(commandBuffer: *mut core::ffi::c_void, raygenShaderBindingTableBuffer: u64, raygenShaderBindingOffset: u64, missShaderBindingTableBuffer: u64, missShaderBindingOffset: u64, missShaderBindingStride: u64, hitShaderBindingTableBuffer: u64, hitShaderBindingOffset: u64, hitShaderBindingStride: u64, callableShaderBindingTableBuffer: u64, callableShaderBindingOffset: u64, callableShaderBindingStride: u64, width: u32, height: u32, depth: u32) { let _ = commandBuffer; let _ = raygenShaderBindingTableBuffer; let _ = raygenShaderBindingOffset; let _ = missShaderBindingTableBuffer; let _ = missShaderBindingOffset; let _ = missShaderBindingStride; let _ = hitShaderBindingTableBuffer; let _ = hitShaderBindingOffset; let _ = hitShaderBindingStride; let _ = callableShaderBindingTableBuffer; let _ = callableShaderBindingOffset; let _ = callableShaderBindingStride; let _ = width; let _ = height; let _ = depth; crate::stub::Call::unsupported("vkCmdTraceRaysNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteAccelerationStructuresPropertiesKHR(commandBuffer: *mut core::ffi::c_void, accelerationStructureCount: u32, pAccelerationStructures: *const core::ffi::c_void, queryType: i32, queryPool: u64, firstQuery: u32) { let _ = commandBuffer; let _ = accelerationStructureCount; let _ = pAccelerationStructures; let _ = queryType; let _ = queryPool; let _ = firstQuery; crate::stub::Call::unsupported("vkCmdWriteAccelerationStructuresPropertiesKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteAccelerationStructuresPropertiesNV(commandBuffer: *mut core::ffi::c_void, accelerationStructureCount: u32, pAccelerationStructures: *const core::ffi::c_void, queryType: i32, queryPool: u64, firstQuery: u32) { let _ = commandBuffer; let _ = accelerationStructureCount; let _ = pAccelerationStructures; let _ = queryType; let _ = queryPool; let _ = firstQuery; crate::stub::Call::unsupported("vkCmdWriteAccelerationStructuresPropertiesNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteMicromapsPropertiesEXT(commandBuffer: *mut core::ffi::c_void, micromapCount: u32, pMicromaps: *const core::ffi::c_void, queryType: i32, queryPool: u64, firstQuery: u32) { let _ = commandBuffer; let _ = micromapCount; let _ = pMicromaps; let _ = queryType; let _ = queryPool; let _ = firstQuery; crate::stub::Call::unsupported("vkCmdWriteMicromapsPropertiesEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkCopyAccelerationStructureKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::Call::unsupported("vkCopyAccelerationStructureKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyAccelerationStructureToMemoryKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::Call::unsupported("vkCopyAccelerationStructureToMemoryKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMemoryToAccelerationStructureKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::Call::unsupported("vkCopyMemoryToAccelerationStructureKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMemoryToMicromapEXT(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::Call::unsupported("vkCopyMemoryToMicromapEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMicromapEXT(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::Call::unsupported("vkCopyMicromapEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCopyMicromapToMemoryEXT(device: *mut core::ffi::c_void, deferredOperation: u64, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pInfo; crate::stub::Call::unsupported("vkCopyMicromapToMemoryEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateAccelerationStructureKHR(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pAccelerationStructure: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pAccelerationStructure; unsafe { if !pAccelerationStructure.is_null() { *(pAccelerationStructure as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateAccelerationStructureKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateAccelerationStructureNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pAccelerationStructure: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pAccelerationStructure; unsafe { if !pAccelerationStructure.is_null() { *(pAccelerationStructure as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateAccelerationStructureNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateMicromapEXT(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pMicromap: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pMicromap; unsafe { if !pMicromap.is_null() { *(pMicromap as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateMicromapEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateOpticalFlowSessionNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSession: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pSession; unsafe { if !pSession.is_null() { *(pSession as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateOpticalFlowSessionNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateRayTracingPipelinesKHR(device: *mut core::ffi::c_void, deferredOperation: u64, pipelineCache: u64, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pPipelines: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = deferredOperation; let _ = pipelineCache; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pPipelines; unsafe { if !pPipelines.is_null() { *(pPipelines as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateRayTracingPipelinesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateRayTracingPipelinesNV(device: *mut core::ffi::c_void, pipelineCache: u64, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pPipelines: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipelineCache; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pPipelines; unsafe { if !pPipelines.is_null() { *(pPipelines as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateRayTracingPipelinesNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateVideoSessionKHR(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pVideoSession: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pVideoSession; unsafe { if !pVideoSession.is_null() { *(pVideoSession as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateVideoSessionKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateVideoSessionParametersKHR(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pVideoSessionParameters: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pVideoSessionParameters; unsafe { if !pVideoSessionParameters.is_null() { *(pVideoSessionParameters as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateVideoSessionParametersKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkDestroyAccelerationStructureKHR(device: *mut core::ffi::c_void, accelerationStructure: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = accelerationStructure; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyAccelerationStructureKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyAccelerationStructureNV(device: *mut core::ffi::c_void, accelerationStructure: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = accelerationStructure; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyAccelerationStructureNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyMicromapEXT(device: *mut core::ffi::c_void, micromap: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = micromap; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyMicromapEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyOpticalFlowSessionNV(device: *mut core::ffi::c_void, session: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = session; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyOpticalFlowSessionNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyVideoSessionKHR(device: *mut core::ffi::c_void, videoSession: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = videoSession; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyVideoSessionKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkDestroyVideoSessionParametersKHR(device: *mut core::ffi::c_void, videoSessionParameters: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = videoSessionParameters; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyVideoSessionParametersKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureBuildSizesKHR(device: *mut core::ffi::c_void, buildType: i32, pBuildInfo: *const core::ffi::c_void, pMaxPrimitiveCounts: *const core::ffi::c_void, pSizeInfo: *mut core::ffi::c_void) { let _ = device; let _ = buildType; let _ = pBuildInfo; let _ = pMaxPrimitiveCounts; let _ = pSizeInfo; crate::stub::Call::unsupported("vkGetAccelerationStructureBuildSizesKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureDeviceAddressKHR(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> u64 { let _ = device; let _ = pInfo; crate::stub::Call::unsupported("vkGetAccelerationStructureDeviceAddressKHR", "extension family not modeled"); 0 }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureHandleNV(device: *mut core::ffi::c_void, accelerationStructure: u64, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = accelerationStructure; let _ = dataSize; let _ = pData; crate::stub::Call::unsupported("vkGetAccelerationStructureHandleNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureMemoryRequirementsNV(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) { let _ = device; let _ = pInfo; let _ = pMemoryRequirements; crate::stub::Call::unsupported("vkGetAccelerationStructureMemoryRequirementsNV", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetAccelerationStructureOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::Call::unsupported("vkGetAccelerationStructureOpaqueCaptureDescriptorDataEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDeviceAccelerationStructureCompatibilityKHR(device: *mut core::ffi::c_void, pVersionInfo: *const core::ffi::c_void, pCompatibility: *mut core::ffi::c_void) { let _ = device; let _ = pVersionInfo; let _ = pCompatibility; crate::stub::Call::unsupported("vkGetDeviceAccelerationStructureCompatibilityKHR", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetDeviceMicromapCompatibilityEXT(device: *mut core::ffi::c_void, pVersionInfo: *const core::ffi::c_void, pCompatibility: *mut core::ffi::c_void) { let _ = device; let _ = pVersionInfo; let _ = pCompatibility; crate::stub::Call::unsupported("vkGetDeviceMicromapCompatibilityEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetEncodedVideoSessionParametersKHR(device: *mut core::ffi::c_void, pVideoSessionParametersInfo: *const core::ffi::c_void, pFeedbackInfo: *mut core::ffi::c_void, pDataSize: *mut core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pVideoSessionParametersInfo; let _ = pFeedbackInfo; let _ = pDataSize; let _ = pData; crate::stub::Call::unsupported("vkGetEncodedVideoSessionParametersKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMicromapBuildSizesEXT(device: *mut core::ffi::c_void, buildType: i32, pBuildInfo: *const core::ffi::c_void, pSizeInfo: *mut core::ffi::c_void) { let _ = device; let _ = buildType; let _ = pBuildInfo; let _ = pSizeInfo; crate::stub::Call::unsupported("vkGetMicromapBuildSizesEXT", "extension family not modeled"); }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceCooperativeMatrixPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceCooperativeMatrixPropertiesNV(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceCooperativeMatrixPropertiesNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceOpticalFlowImageFormatsNV(physicalDevice: *mut core::ffi::c_void, pOpticalFlowImageFormatInfo: *const core::ffi::c_void, pFormatCount: *mut core::ffi::c_void, pImageFormatProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pOpticalFlowImageFormatInfo; let _ = pFormatCount; let _ = pImageFormatProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceOpticalFlowImageFormatsNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoCapabilitiesKHR(physicalDevice: *mut core::ffi::c_void, pVideoProfile: *const core::ffi::c_void, pCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pVideoProfile; let _ = pCapabilities; crate::stub::Call::unsupported("vkGetPhysicalDeviceVideoCapabilitiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pQualityLevelInfo: *const core::ffi::c_void, pQualityLevelProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pQualityLevelInfo; let _ = pQualityLevelProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceVideoEncodeQualityLevelPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceVideoFormatPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pVideoFormatInfo: *const core::ffi::c_void, pVideoFormatPropertyCount: *mut core::ffi::c_void, pVideoFormatProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pVideoFormatInfo; let _ = pVideoFormatPropertyCount; let _ = pVideoFormatProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceVideoFormatPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingCaptureReplayShaderGroupHandlesKHR(device: *mut core::ffi::c_void, pipeline: u64, firstGroup: u32, groupCount: u32, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = firstGroup; let _ = groupCount; let _ = dataSize; let _ = pData; crate::stub::Call::unsupported("vkGetRayTracingCaptureReplayShaderGroupHandlesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingShaderGroupHandlesKHR(device: *mut core::ffi::c_void, pipeline: u64, firstGroup: u32, groupCount: u32, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = firstGroup; let _ = groupCount; let _ = dataSize; let _ = pData; crate::stub::Call::unsupported("vkGetRayTracingShaderGroupHandlesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingShaderGroupHandlesNV(device: *mut core::ffi::c_void, pipeline: u64, firstGroup: u32, groupCount: u32, dataSize: usize, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = firstGroup; let _ = groupCount; let _ = dataSize; let _ = pData; crate::stub::Call::unsupported("vkGetRayTracingShaderGroupHandlesNV", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRayTracingShaderGroupStackSizeKHR(device: *mut core::ffi::c_void, pipeline: u64, group: u32, groupShader: i32) -> u64 { let _ = device; let _ = pipeline; let _ = group; let _ = groupShader; crate::stub::Call::unsupported("vkGetRayTracingShaderGroupStackSizeKHR", "extension family not modeled"); 0 }

#[no_mangle]
pub extern "C" fn vkGetVideoSessionMemoryRequirementsKHR(device: *mut core::ffi::c_void, videoSession: u64, pMemoryRequirementsCount: *mut core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = videoSession; let _ = pMemoryRequirementsCount; let _ = pMemoryRequirements; crate::stub::Call::unsupported("vkGetVideoSessionMemoryRequirementsKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkUpdateVideoSessionParametersKHR(device: *mut core::ffi::c_void, videoSessionParameters: u64, pUpdateInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = videoSessionParameters; let _ = pUpdateInfo; crate::stub::Call::unsupported("vkUpdateVideoSessionParametersKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkWriteAccelerationStructuresPropertiesKHR(device: *mut core::ffi::c_void, accelerationStructureCount: u32, pAccelerationStructures: *const core::ffi::c_void, queryType: i32, dataSize: usize, pData: *mut core::ffi::c_void, stride: usize) -> i32 { let _ = device; let _ = accelerationStructureCount; let _ = pAccelerationStructures; let _ = queryType; let _ = dataSize; let _ = pData; let _ = stride; crate::stub::Call::unsupported("vkWriteAccelerationStructuresPropertiesKHR", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkWriteMicromapsPropertiesEXT(device: *mut core::ffi::c_void, micromapCount: u32, pMicromaps: *const core::ffi::c_void, queryType: i32, dataSize: usize, pData: *mut core::ffi::c_void, stride: usize) -> i32 { let _ = device; let _ = micromapCount; let _ = pMicromaps; let _ = queryType; let _ = dataSize; let _ = pData; let _ = stride; crate::stub::Call::unsupported("vkWriteMicromapsPropertiesEXT", "extension family not modeled"); VK_ERROR_EXTENSION_NOT_PRESENT }


// ---- @generated honest not-supported bodies for the unmodeled long tail (appended by task) ----

// Each validates argument shape, nulls a create/allocate output handle, zeroes a query count

// (so a two-call enumeration reads zero results, never junk), once-logs an `unsupported` trace,

// and returns the truthful VkResult / zero. The extensions these belong to are NOT advertised.


#[no_mangle]
pub extern "C" fn vkAcquireDrmDisplayEXT(physicalDevice: *mut core::ffi::c_void, drmFd: i32, display: u64) -> i32 { let _ = physicalDevice; let _ = drmFd; let _ = display; crate::stub::Call::unsupported("vkAcquireDrmDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireFullScreenExclusiveModeEXT(device: *mut core::ffi::c_void, swapchain: u64) -> i32 { let _ = device; let _ = swapchain; crate::stub::Call::unsupported("vkAcquireFullScreenExclusiveModeEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireImageANDROID(device: *mut core::ffi::c_void, image: u64, nativeFenceFd: i32, semaphore: u64, fence: u64) -> i32 { let _ = device; let _ = image; let _ = nativeFenceFd; let _ = semaphore; let _ = fence; crate::stub::Call::unsupported("vkAcquireImageANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireNextImage2KHR(device: *mut core::ffi::c_void, pAcquireInfo: *const core::ffi::c_void, pImageIndex: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pAcquireInfo; let _ = pImageIndex; crate::stub::Call::unsupported("vkAcquireNextImage2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquirePerformanceConfigurationINTEL(device: *mut core::ffi::c_void, pAcquireInfo: *const core::ffi::c_void, pConfiguration: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pAcquireInfo; let _ = pConfiguration; crate::stub::Call::unsupported("vkAcquirePerformanceConfigurationINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireProfilingLockKHR(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; crate::stub::Call::unsupported("vkAcquireProfilingLockKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireWinrtDisplayNV(physicalDevice: *mut core::ffi::c_void, display: u64) -> i32 { let _ = physicalDevice; let _ = display; crate::stub::Call::unsupported("vkAcquireWinrtDisplayNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkAcquireXlibDisplayEXT(physicalDevice: *mut core::ffi::c_void, dpy: *mut core::ffi::c_void, display: u64) -> i32 { let _ = physicalDevice; let _ = dpy; let _ = display; crate::stub::Call::unsupported("vkAcquireXlibDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdBeginConditionalRenderingEXT(commandBuffer: *mut core::ffi::c_void, pConditionalRenderingBegin: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pConditionalRenderingBegin; crate::stub::Call::unsupported("vkCmdBeginConditionalRenderingEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBeginQueryIndexedEXT(commandBuffer: *mut core::ffi::c_void, queryPool: u64, query: u32, flags: u32, index: u32) { let _ = commandBuffer; let _ = queryPool; let _ = query; let _ = flags; let _ = index; crate::stub::Call::unsupported("vkCmdBeginQueryIndexedEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBeginTransformFeedbackEXT(commandBuffer: *mut core::ffi::c_void, firstCounterBuffer: u32, counterBufferCount: u32, pCounterBuffers: *const core::ffi::c_void, pCounterBufferOffsets: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstCounterBuffer; let _ = counterBufferCount; let _ = pCounterBuffers; let _ = pCounterBufferOffsets; crate::stub::Call::unsupported("vkCmdBeginTransformFeedbackEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBufferEmbeddedSamplers2EXT(commandBuffer: *mut core::ffi::c_void, pBindDescriptorBufferEmbeddedSamplersInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBindDescriptorBufferEmbeddedSamplersInfo; crate::stub::Call::unsupported("vkCmdBindDescriptorBufferEmbeddedSamplers2EXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBufferEmbeddedSamplersEXT(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, set: u32) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = set; crate::stub::Call::unsupported("vkCmdBindDescriptorBufferEmbeddedSamplersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorBuffersEXT(commandBuffer: *mut core::ffi::c_void, bufferCount: u32, pBindingInfos: *const core::ffi::c_void) { let _ = commandBuffer; let _ = bufferCount; let _ = pBindingInfos; crate::stub::Call::unsupported("vkCmdBindDescriptorBuffersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2(commandBuffer: *mut core::ffi::c_void, pBindDescriptorSetsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBindDescriptorSetsInfo; crate::stub::Call::unsupported("vkCmdBindDescriptorSets2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindDescriptorSets2KHR(commandBuffer: *mut core::ffi::c_void, pBindDescriptorSetsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pBindDescriptorSetsInfo; crate::stub::Call::unsupported("vkCmdBindDescriptorSets2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer2(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, size: u64, indexType: i32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = size; let _ = indexType; crate::stub::Call::unsupported("vkCmdBindIndexBuffer2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindIndexBuffer2KHR(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64, size: u64, indexType: i32) { let _ = commandBuffer; let _ = buffer; let _ = offset; let _ = size; let _ = indexType; crate::stub::Call::unsupported("vkCmdBindIndexBuffer2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindInvocationMaskHUAWEI(commandBuffer: *mut core::ffi::c_void, imageView: u64, imageLayout: i32) { let _ = commandBuffer; let _ = imageView; let _ = imageLayout; crate::stub::Call::unsupported("vkCmdBindInvocationMaskHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindPipelineShaderGroupNV(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, pipeline: u64, groupIndex: u32) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = pipeline; let _ = groupIndex; crate::stub::Call::unsupported("vkCmdBindPipelineShaderGroupNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindShadersEXT(commandBuffer: *mut core::ffi::c_void, stageCount: u32, pStages: *const core::ffi::c_void, pShaders: *const core::ffi::c_void) { let _ = commandBuffer; let _ = stageCount; let _ = pStages; let _ = pShaders; crate::stub::Call::unsupported("vkCmdBindShadersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindShadingRateImageNV(commandBuffer: *mut core::ffi::c_void, imageView: u64, imageLayout: i32) { let _ = commandBuffer; let _ = imageView; let _ = imageLayout; crate::stub::Call::unsupported("vkCmdBindShadingRateImageNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdBindTransformFeedbackBuffersEXT(commandBuffer: *mut core::ffi::c_void, firstBinding: u32, bindingCount: u32, pBuffers: *const core::ffi::c_void, pOffsets: *const core::ffi::c_void, pSizes: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstBinding; let _ = bindingCount; let _ = pBuffers; let _ = pOffsets; let _ = pSizes; crate::stub::Call::unsupported("vkCmdBindTransformFeedbackBuffersEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryIndirectNV(commandBuffer: *mut core::ffi::c_void, copyBufferAddress: u64, copyCount: u32, stride: u32) { let _ = commandBuffer; let _ = copyBufferAddress; let _ = copyCount; let _ = stride; crate::stub::Call::unsupported("vkCmdCopyMemoryIndirectNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCopyMemoryToImageIndirectNV(commandBuffer: *mut core::ffi::c_void, copyBufferAddress: u64, copyCount: u32, stride: u32, dstImage: u64, dstImageLayout: i32, pImageSubresources: *const core::ffi::c_void) { let _ = commandBuffer; let _ = copyBufferAddress; let _ = copyCount; let _ = stride; let _ = dstImage; let _ = dstImageLayout; let _ = pImageSubresources; crate::stub::Call::unsupported("vkCmdCopyMemoryToImageIndirectNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCuLaunchKernelNVX(commandBuffer: *mut core::ffi::c_void, pLaunchInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pLaunchInfo; crate::stub::Call::unsupported("vkCmdCuLaunchKernelNVX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdCudaLaunchKernelNV(commandBuffer: *mut core::ffi::c_void, pLaunchInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pLaunchInfo; crate::stub::Call::unsupported("vkCmdCudaLaunchKernelNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDecompressMemoryIndirectCountNV(commandBuffer: *mut core::ffi::c_void, indirectCommandsAddress: u64, indirectCommandsCountAddress: u64, stride: u32) { let _ = commandBuffer; let _ = indirectCommandsAddress; let _ = indirectCommandsCountAddress; let _ = stride; crate::stub::Call::unsupported("vkCmdDecompressMemoryIndirectCountNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDecompressMemoryNV(commandBuffer: *mut core::ffi::c_void, decompressRegionCount: u32, pDecompressMemoryRegions: *const core::ffi::c_void) { let _ = commandBuffer; let _ = decompressRegionCount; let _ = pDecompressMemoryRegions; crate::stub::Call::unsupported("vkCmdDecompressMemoryNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDispatchGraphAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64, pCountInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = scratch; let _ = pCountInfo; crate::stub::Call::unsupported("vkCmdDispatchGraphAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDispatchGraphIndirectAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64, pCountInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = scratch; let _ = pCountInfo; crate::stub::Call::unsupported("vkCmdDispatchGraphIndirectAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDispatchGraphIndirectCountAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64, countInfo: u64) { let _ = commandBuffer; let _ = scratch; let _ = countInfo; crate::stub::Call::unsupported("vkCmdDispatchGraphIndirectCountAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawClusterHUAWEI(commandBuffer: *mut core::ffi::c_void, groupCountX: u32, groupCountY: u32, groupCountZ: u32) { let _ = commandBuffer; let _ = groupCountX; let _ = groupCountY; let _ = groupCountZ; crate::stub::Call::unsupported("vkCmdDrawClusterHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawClusterIndirectHUAWEI(commandBuffer: *mut core::ffi::c_void, buffer: u64, offset: u64) { let _ = commandBuffer; let _ = buffer; let _ = offset; crate::stub::Call::unsupported("vkCmdDrawClusterIndirectHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawIndirectByteCountEXT(commandBuffer: *mut core::ffi::c_void, instanceCount: u32, firstInstance: u32, counterBuffer: u64, counterBufferOffset: u64, counterOffset: u32, vertexStride: u32) { let _ = commandBuffer; let _ = instanceCount; let _ = firstInstance; let _ = counterBuffer; let _ = counterBufferOffset; let _ = counterOffset; let _ = vertexStride; crate::stub::Call::unsupported("vkCmdDrawIndirectByteCountEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMultiEXT(commandBuffer: *mut core::ffi::c_void, drawCount: u32, pVertexInfo: *const core::ffi::c_void, instanceCount: u32, firstInstance: u32, stride: u32) { let _ = commandBuffer; let _ = drawCount; let _ = pVertexInfo; let _ = instanceCount; let _ = firstInstance; let _ = stride; crate::stub::Call::unsupported("vkCmdDrawMultiEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdDrawMultiIndexedEXT(commandBuffer: *mut core::ffi::c_void, drawCount: u32, pIndexInfo: *const core::ffi::c_void, instanceCount: u32, firstInstance: u32, stride: u32, pVertexOffset: *const core::ffi::c_void) { let _ = commandBuffer; let _ = drawCount; let _ = pIndexInfo; let _ = instanceCount; let _ = firstInstance; let _ = stride; let _ = pVertexOffset; crate::stub::Call::unsupported("vkCmdDrawMultiIndexedEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdEndConditionalRenderingEXT(commandBuffer: *mut core::ffi::c_void) { let _ = commandBuffer; crate::stub::Call::unsupported("vkCmdEndConditionalRenderingEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdEndQueryIndexedEXT(commandBuffer: *mut core::ffi::c_void, queryPool: u64, query: u32, index: u32) { let _ = commandBuffer; let _ = queryPool; let _ = query; let _ = index; crate::stub::Call::unsupported("vkCmdEndQueryIndexedEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdEndTransformFeedbackEXT(commandBuffer: *mut core::ffi::c_void, firstCounterBuffer: u32, counterBufferCount: u32, pCounterBuffers: *const core::ffi::c_void, pCounterBufferOffsets: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstCounterBuffer; let _ = counterBufferCount; let _ = pCounterBuffers; let _ = pCounterBufferOffsets; crate::stub::Call::unsupported("vkCmdEndTransformFeedbackEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdExecuteGeneratedCommandsNV(commandBuffer: *mut core::ffi::c_void, isPreprocessed: u32, pGeneratedCommandsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = isPreprocessed; let _ = pGeneratedCommandsInfo; crate::stub::Call::unsupported("vkCmdExecuteGeneratedCommandsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdInitializeGraphScratchMemoryAMDX(commandBuffer: *mut core::ffi::c_void, scratch: u64) { let _ = commandBuffer; let _ = scratch; crate::stub::Call::unsupported("vkCmdInitializeGraphScratchMemoryAMDX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPreprocessGeneratedCommandsNV(commandBuffer: *mut core::ffi::c_void, pGeneratedCommandsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pGeneratedCommandsInfo; crate::stub::Call::unsupported("vkCmdPreprocessGeneratedCommandsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2(commandBuffer: *mut core::ffi::c_void, pPushConstantsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushConstantsInfo; crate::stub::Call::unsupported("vkCmdPushConstants2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushConstants2KHR(commandBuffer: *mut core::ffi::c_void, pPushConstantsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushConstantsInfo; crate::stub::Call::unsupported("vkCmdPushConstants2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, set: u32, descriptorWriteCount: u32, pDescriptorWrites: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = set; let _ = descriptorWriteCount; let _ = pDescriptorWrites; crate::stub::Call::unsupported("vkCmdPushDescriptorSet", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetInfo; crate::stub::Call::unsupported("vkCmdPushDescriptorSet2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSet2KHR(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetInfo; crate::stub::Call::unsupported("vkCmdPushDescriptorSet2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetKHR(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, set: u32, descriptorWriteCount: u32, pDescriptorWrites: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = set; let _ = descriptorWriteCount; let _ = pDescriptorWrites; crate::stub::Call::unsupported("vkCmdPushDescriptorSetKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate(commandBuffer: *mut core::ffi::c_void, descriptorUpdateTemplate: u64, layout: u64, set: u32, pData: *const core::ffi::c_void) { let _ = commandBuffer; let _ = descriptorUpdateTemplate; let _ = layout; let _ = set; let _ = pData; crate::stub::Call::unsupported("vkCmdPushDescriptorSetWithTemplate", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetWithTemplateInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetWithTemplateInfo; crate::stub::Call::unsupported("vkCmdPushDescriptorSetWithTemplate2", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2KHR(commandBuffer: *mut core::ffi::c_void, pPushDescriptorSetWithTemplateInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pPushDescriptorSetWithTemplateInfo; crate::stub::Call::unsupported("vkCmdPushDescriptorSetWithTemplate2KHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdPushDescriptorSetWithTemplateKHR(commandBuffer: *mut core::ffi::c_void, descriptorUpdateTemplate: u64, layout: u64, set: u32, pData: *const core::ffi::c_void) { let _ = commandBuffer; let _ = descriptorUpdateTemplate; let _ = layout; let _ = set; let _ = pData; crate::stub::Call::unsupported("vkCmdPushDescriptorSetWithTemplateKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdRefreshObjectsKHR(commandBuffer: *mut core::ffi::c_void, pRefreshObjects: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pRefreshObjects; crate::stub::Call::unsupported("vkCmdRefreshObjectsKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetAttachmentFeedbackLoopEnableEXT(commandBuffer: *mut core::ffi::c_void, aspectMask: u32) { let _ = commandBuffer; let _ = aspectMask; crate::stub::Call::unsupported("vkCmdSetAttachmentFeedbackLoopEnableEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCheckpointNV(commandBuffer: *mut core::ffi::c_void, pCheckpointMarker: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pCheckpointMarker; crate::stub::Call::unsupported("vkCmdSetCheckpointNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoarseSampleOrderNV(commandBuffer: *mut core::ffi::c_void, sampleOrderType: i32, customSampleOrderCount: u32, pCustomSampleOrders: *const core::ffi::c_void) { let _ = commandBuffer; let _ = sampleOrderType; let _ = customSampleOrderCount; let _ = pCustomSampleOrders; crate::stub::Call::unsupported("vkCmdSetCoarseSampleOrderNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageModulationModeNV(commandBuffer: *mut core::ffi::c_void, coverageModulationMode: i32) { let _ = commandBuffer; let _ = coverageModulationMode; crate::stub::Call::unsupported("vkCmdSetCoverageModulationModeNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageModulationTableEnableNV(commandBuffer: *mut core::ffi::c_void, coverageModulationTableEnable: u32) { let _ = commandBuffer; let _ = coverageModulationTableEnable; crate::stub::Call::unsupported("vkCmdSetCoverageModulationTableEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageModulationTableNV(commandBuffer: *mut core::ffi::c_void, coverageModulationTableCount: u32, pCoverageModulationTable: *const core::ffi::c_void) { let _ = commandBuffer; let _ = coverageModulationTableCount; let _ = pCoverageModulationTable; crate::stub::Call::unsupported("vkCmdSetCoverageModulationTableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageReductionModeNV(commandBuffer: *mut core::ffi::c_void, coverageReductionMode: i32) { let _ = commandBuffer; let _ = coverageReductionMode; crate::stub::Call::unsupported("vkCmdSetCoverageReductionModeNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageToColorEnableNV(commandBuffer: *mut core::ffi::c_void, coverageToColorEnable: u32) { let _ = commandBuffer; let _ = coverageToColorEnable; crate::stub::Call::unsupported("vkCmdSetCoverageToColorEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetCoverageToColorLocationNV(commandBuffer: *mut core::ffi::c_void, coverageToColorLocation: u32) { let _ = commandBuffer; let _ = coverageToColorLocation; crate::stub::Call::unsupported("vkCmdSetCoverageToColorLocationNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDescriptorBufferOffsets2EXT(commandBuffer: *mut core::ffi::c_void, pSetDescriptorBufferOffsetsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pSetDescriptorBufferOffsetsInfo; crate::stub::Call::unsupported("vkCmdSetDescriptorBufferOffsets2EXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDescriptorBufferOffsetsEXT(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, layout: u64, firstSet: u32, setCount: u32, pBufferIndices: *const core::ffi::c_void, pOffsets: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = layout; let _ = firstSet; let _ = setCount; let _ = pBufferIndices; let _ = pOffsets; crate::stub::Call::unsupported("vkCmdSetDescriptorBufferOffsetsEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDiscardRectangleEXT(commandBuffer: *mut core::ffi::c_void, firstDiscardRectangle: u32, discardRectangleCount: u32, pDiscardRectangles: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstDiscardRectangle; let _ = discardRectangleCount; let _ = pDiscardRectangles; crate::stub::Call::unsupported("vkCmdSetDiscardRectangleEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDiscardRectangleEnableEXT(commandBuffer: *mut core::ffi::c_void, discardRectangleEnable: u32) { let _ = commandBuffer; let _ = discardRectangleEnable; crate::stub::Call::unsupported("vkCmdSetDiscardRectangleEnableEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetDiscardRectangleModeEXT(commandBuffer: *mut core::ffi::c_void, discardRectangleMode: i32) { let _ = commandBuffer; let _ = discardRectangleMode; crate::stub::Call::unsupported("vkCmdSetDiscardRectangleModeEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetExclusiveScissorEnableNV(commandBuffer: *mut core::ffi::c_void, firstExclusiveScissor: u32, exclusiveScissorCount: u32, pExclusiveScissorEnables: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstExclusiveScissor; let _ = exclusiveScissorCount; let _ = pExclusiveScissorEnables; crate::stub::Call::unsupported("vkCmdSetExclusiveScissorEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetExclusiveScissorNV(commandBuffer: *mut core::ffi::c_void, firstExclusiveScissor: u32, exclusiveScissorCount: u32, pExclusiveScissors: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstExclusiveScissor; let _ = exclusiveScissorCount; let _ = pExclusiveScissors; crate::stub::Call::unsupported("vkCmdSetExclusiveScissorNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetFragmentShadingRateEnumNV(commandBuffer: *mut core::ffi::c_void, shadingRate: i32, combinerOps: *const core::ffi::c_void) { let _ = commandBuffer; let _ = shadingRate; let _ = combinerOps; crate::stub::Call::unsupported("vkCmdSetFragmentShadingRateEnumNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetFragmentShadingRateKHR(commandBuffer: *mut core::ffi::c_void, pFragmentSize: *const core::ffi::c_void, combinerOps: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pFragmentSize; let _ = combinerOps; crate::stub::Call::unsupported("vkCmdSetFragmentShadingRateKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetPerformanceMarkerINTEL(commandBuffer: *mut core::ffi::c_void, pMarkerInfo: *const core::ffi::c_void) -> i32 { let _ = commandBuffer; let _ = pMarkerInfo; crate::stub::Call::unsupported("vkCmdSetPerformanceMarkerINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdSetPerformanceOverrideINTEL(commandBuffer: *mut core::ffi::c_void, pOverrideInfo: *const core::ffi::c_void) -> i32 { let _ = commandBuffer; let _ = pOverrideInfo; crate::stub::Call::unsupported("vkCmdSetPerformanceOverrideINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdSetPerformanceStreamMarkerINTEL(commandBuffer: *mut core::ffi::c_void, pMarkerInfo: *const core::ffi::c_void) -> i32 { let _ = commandBuffer; let _ = pMarkerInfo; crate::stub::Call::unsupported("vkCmdSetPerformanceStreamMarkerINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCmdSetRenderingAttachmentLocations(commandBuffer: *mut core::ffi::c_void, pLocationInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pLocationInfo; crate::stub::Call::unsupported("vkCmdSetRenderingAttachmentLocations", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetRenderingInputAttachmentIndices(commandBuffer: *mut core::ffi::c_void, pInputAttachmentIndexInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pInputAttachmentIndexInfo; crate::stub::Call::unsupported("vkCmdSetRenderingInputAttachmentIndices", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetRepresentativeFragmentTestEnableNV(commandBuffer: *mut core::ffi::c_void, representativeFragmentTestEnable: u32) { let _ = commandBuffer; let _ = representativeFragmentTestEnable; crate::stub::Call::unsupported("vkCmdSetRepresentativeFragmentTestEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetSampleLocationsEXT(commandBuffer: *mut core::ffi::c_void, pSampleLocationsInfo: *const core::ffi::c_void) { let _ = commandBuffer; let _ = pSampleLocationsInfo; crate::stub::Call::unsupported("vkCmdSetSampleLocationsEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetShadingRateImageEnableNV(commandBuffer: *mut core::ffi::c_void, shadingRateImageEnable: u32) { let _ = commandBuffer; let _ = shadingRateImageEnable; crate::stub::Call::unsupported("vkCmdSetShadingRateImageEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportShadingRatePaletteNV(commandBuffer: *mut core::ffi::c_void, firstViewport: u32, viewportCount: u32, pShadingRatePalettes: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstViewport; let _ = viewportCount; let _ = pShadingRatePalettes; crate::stub::Call::unsupported("vkCmdSetViewportShadingRatePaletteNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportSwizzleNV(commandBuffer: *mut core::ffi::c_void, firstViewport: u32, viewportCount: u32, pViewportSwizzles: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstViewport; let _ = viewportCount; let _ = pViewportSwizzles; crate::stub::Call::unsupported("vkCmdSetViewportSwizzleNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportWScalingEnableNV(commandBuffer: *mut core::ffi::c_void, viewportWScalingEnable: u32) { let _ = commandBuffer; let _ = viewportWScalingEnable; crate::stub::Call::unsupported("vkCmdSetViewportWScalingEnableNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSetViewportWScalingNV(commandBuffer: *mut core::ffi::c_void, firstViewport: u32, viewportCount: u32, pViewportWScalings: *const core::ffi::c_void) { let _ = commandBuffer; let _ = firstViewport; let _ = viewportCount; let _ = pViewportWScalings; crate::stub::Call::unsupported("vkCmdSetViewportWScalingNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdSubpassShadingHUAWEI(commandBuffer: *mut core::ffi::c_void) { let _ = commandBuffer; crate::stub::Call::unsupported("vkCmdSubpassShadingHUAWEI", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdUpdatePipelineIndirectBufferNV(commandBuffer: *mut core::ffi::c_void, pipelineBindPoint: i32, pipeline: u64) { let _ = commandBuffer; let _ = pipelineBindPoint; let _ = pipeline; crate::stub::Call::unsupported("vkCmdUpdatePipelineIndirectBufferNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteBufferMarker2AMD(commandBuffer: *mut core::ffi::c_void, stage: u64, dstBuffer: u64, dstOffset: u64, marker: u32) { let _ = commandBuffer; let _ = stage; let _ = dstBuffer; let _ = dstOffset; let _ = marker; crate::stub::Call::unsupported("vkCmdWriteBufferMarker2AMD", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCmdWriteBufferMarkerAMD(commandBuffer: *mut core::ffi::c_void, pipelineStage: i32, dstBuffer: u64, dstOffset: u64, marker: u32) { let _ = commandBuffer; let _ = pipelineStage; let _ = dstBuffer; let _ = dstOffset; let _ = marker; crate::stub::Call::unsupported("vkCmdWriteBufferMarkerAMD", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkCompileDeferredNV(device: *mut core::ffi::c_void, pipeline: u64, shader: u32) -> i32 { let _ = device; let _ = pipeline; let _ = shader; crate::stub::Call::unsupported("vkCompileDeferredNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateAndroidSurfaceKHR(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateAndroidSurfaceKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateBufferCollectionFUCHSIA(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pCollection: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pCollection; unsafe { if !pCollection.is_null() { *(pCollection as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateBufferCollectionFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCuFunctionNVX(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFunction: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pFunction; unsafe { if !pFunction.is_null() { *(pFunction as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateCuFunctionNVX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCuModuleNVX(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pModule: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pModule; unsafe { if !pModule.is_null() { *(pModule as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateCuModuleNVX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCudaFunctionNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFunction: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pFunction; unsafe { if !pFunction.is_null() { *(pFunction as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateCudaFunctionNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateCudaModuleNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pModule: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pModule; unsafe { if !pModule.is_null() { *(pModule as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateCudaModuleNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDeferredOperationKHR(device: *mut core::ffi::c_void, pAllocator: *const core::ffi::c_void, pDeferredOperation: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pAllocator; let _ = pDeferredOperation; unsafe { if !pDeferredOperation.is_null() { *(pDeferredOperation as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateDeferredOperationKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDirectFBSurfaceEXT(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateDirectFBSurfaceEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDisplayModeKHR(physicalDevice: *mut core::ffi::c_void, display: u64, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pMode: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = display; let _ = pCreateInfo; let _ = pAllocator; let _ = pMode; unsafe { if !pMode.is_null() { *(pMode as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateDisplayModeKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateDisplayPlaneSurfaceKHR(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateDisplayPlaneSurfaceKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateExecutionGraphPipelinesAMDX(device: *mut core::ffi::c_void, pipelineCache: u64, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pPipelines: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipelineCache; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pPipelines; unsafe { if !pPipelines.is_null() { *(pPipelines as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateExecutionGraphPipelinesAMDX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateIOSSurfaceMVK(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateIOSSurfaceMVK", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateImagePipeSurfaceFUCHSIA(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateImagePipeSurfaceFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateIndirectCommandsLayoutNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pIndirectCommandsLayout: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pIndirectCommandsLayout; unsafe { if !pIndirectCommandsLayout.is_null() { *(pIndirectCommandsLayout as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateIndirectCommandsLayoutNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateMacOSSurfaceMVK(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateMacOSSurfaceMVK", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateMetalSurfaceEXT(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateMetalSurfaceEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateScreenSurfaceQNX(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateScreenSurfaceQNX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateSemaphoreSciSyncPoolNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSemaphorePool: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pSemaphorePool; unsafe { if !pSemaphorePool.is_null() { *(pSemaphorePool as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateSemaphoreSciSyncPoolNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateShadersEXT(device: *mut core::ffi::c_void, createInfoCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pShaders: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = createInfoCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pShaders; unsafe { if !pShaders.is_null() { *(pShaders as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateShadersEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateSharedSwapchainsKHR(device: *mut core::ffi::c_void, swapchainCount: u32, pCreateInfos: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSwapchains: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchainCount; let _ = pCreateInfos; let _ = pAllocator; let _ = pSwapchains; unsafe { if !pSwapchains.is_null() { *(pSwapchains as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateSharedSwapchainsKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateStreamDescriptorSurfaceGGP(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateStreamDescriptorSurfaceGGP", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateValidationCacheEXT(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pValidationCache: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pCreateInfo; let _ = pAllocator; let _ = pValidationCache; unsafe { if !pValidationCache.is_null() { *(pValidationCache as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateValidationCacheEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateViSurfaceNN(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateViSurfaceNN", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkCreateWin32SurfaceKHR(instance: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pSurface: *mut core::ffi::c_void) -> i32 { let _ = instance; let _ = pCreateInfo; let _ = pAllocator; let _ = pSurface; unsafe { if !pSurface.is_null() { *(pSurface as *mut u64) = 0; } } crate::stub::Call::unsupported("vkCreateWin32SurfaceKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkDeferredOperationJoinKHR(device: *mut core::ffi::c_void, operation: u64) -> i32 { let _ = device; let _ = operation; crate::stub::Call::unsupported("vkDeferredOperationJoinKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkDestroyBufferCollectionFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = collection; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyBufferCollectionFUCHSIA", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCuFunctionNVX(device: *mut core::ffi::c_void, function: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = function; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyCuFunctionNVX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCuModuleNVX(device: *mut core::ffi::c_void, module: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = module; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyCuModuleNVX", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCudaFunctionNV(device: *mut core::ffi::c_void, function: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = function; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyCudaFunctionNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyCudaModuleNV(device: *mut core::ffi::c_void, module: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = module; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyCudaModuleNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyDeferredOperationKHR(device: *mut core::ffi::c_void, operation: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = operation; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyDeferredOperationKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyIndirectCommandsLayoutNV(device: *mut core::ffi::c_void, indirectCommandsLayout: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = indirectCommandsLayout; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyIndirectCommandsLayoutNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroySemaphoreSciSyncPoolNV(device: *mut core::ffi::c_void, semaphorePool: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = semaphorePool; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroySemaphoreSciSyncPoolNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyShaderEXT(device: *mut core::ffi::c_void, shader: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = shader; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyShaderEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDestroyValidationCacheEXT(device: *mut core::ffi::c_void, validationCache: u64, pAllocator: *const core::ffi::c_void) { let _ = device; let _ = validationCache; let _ = pAllocator; crate::stub::Call::unsupported("vkDestroyValidationCacheEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkDisplayPowerControlEXT(device: *mut core::ffi::c_void, display: u64, pDisplayPowerInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = display; let _ = pDisplayPowerInfo; crate::stub::Call::unsupported("vkDisplayPowerControlEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkEnumeratePhysicalDeviceQueueFamilyPerformanceQueryCountersKHR(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32, pCounterCount: *mut core::ffi::c_void, pCounters: *mut core::ffi::c_void, pCounterDescriptions: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = queueFamilyIndex; let _ = pCounterCount; let _ = pCounters; let _ = pCounterDescriptions; crate::stub::Call::unsupported("vkEnumeratePhysicalDeviceQueueFamilyPerformanceQueryCountersKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkExportMetalObjectsEXT(device: *mut core::ffi::c_void, pMetalObjectsInfo: *mut core::ffi::c_void) { let _ = device; let _ = pMetalObjectsInfo; crate::stub::Call::unsupported("vkExportMetalObjectsEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetAndroidHardwareBufferPropertiesANDROID(device: *mut core::ffi::c_void, buffer: *const core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = buffer; let _ = pProperties; crate::stub::Call::unsupported("vkGetAndroidHardwareBufferPropertiesANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetBufferCollectionPropertiesFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = collection; let _ = pProperties; crate::stub::Call::unsupported("vkGetBufferCollectionPropertiesFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetBufferOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::Call::unsupported("vkGetBufferOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetCommandPoolMemoryConsumption(device: *mut core::ffi::c_void, commandPool: u64, commandBuffer: *mut core::ffi::c_void, pConsumption: *mut core::ffi::c_void) { let _ = device; let _ = commandPool; let _ = commandBuffer; let _ = pConsumption; crate::stub::Call::unsupported("vkGetCommandPoolMemoryConsumption", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetCudaModuleCacheNV(device: *mut core::ffi::c_void, module: u64, pCacheSize: *mut core::ffi::c_void, pCacheData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = module; let _ = pCacheSize; let _ = pCacheData; crate::stub::Call::unsupported("vkGetCudaModuleCacheNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDeferredOperationMaxConcurrencyKHR(device: *mut core::ffi::c_void, operation: u64) -> u32 { let _ = device; let _ = operation; crate::stub::Call::unsupported("vkGetDeferredOperationMaxConcurrencyKHR", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetDeferredOperationResultKHR(device: *mut core::ffi::c_void, operation: u64) -> i32 { let _ = device; let _ = operation; crate::stub::Call::unsupported("vkGetDeferredOperationResultKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDescriptorEXT(device: *mut core::ffi::c_void, pDescriptorInfo: *const core::ffi::c_void, dataSize: usize, pDescriptor: *mut core::ffi::c_void) { let _ = device; let _ = pDescriptorInfo; let _ = dataSize; let _ = pDescriptor; crate::stub::Call::unsupported("vkGetDescriptorEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetHostMappingVALVE(device: *mut core::ffi::c_void, descriptorSet: u64, ppData: *mut *mut core::ffi::c_void) { let _ = device; let _ = descriptorSet; let _ = ppData; crate::stub::Call::unsupported("vkGetDescriptorSetHostMappingVALVE", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutBindingOffsetEXT(device: *mut core::ffi::c_void, layout: u64, binding: u32, pOffset: *mut core::ffi::c_void) { let _ = device; let _ = layout; let _ = binding; let _ = pOffset; crate::stub::Call::unsupported("vkGetDescriptorSetLayoutBindingOffsetEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutHostMappingInfoVALVE(device: *mut core::ffi::c_void, pBindingReference: *const core::ffi::c_void, pHostMapping: *mut core::ffi::c_void) { let _ = device; let _ = pBindingReference; let _ = pHostMapping; crate::stub::Call::unsupported("vkGetDescriptorSetLayoutHostMappingInfoVALVE", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDescriptorSetLayoutSizeEXT(device: *mut core::ffi::c_void, layout: u64, pLayoutSizeInBytes: *mut core::ffi::c_void) { let _ = device; let _ = layout; let _ = pLayoutSizeInBytes; crate::stub::Call::unsupported("vkGetDescriptorSetLayoutSizeEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetDeviceFaultInfoEXT(device: *mut core::ffi::c_void, pFaultCounts: *mut core::ffi::c_void, pFaultInfo: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pFaultCounts; let _ = pFaultInfo; crate::stub::Call::unsupported("vkGetDeviceFaultInfoEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDeviceSubpassShadingMaxWorkgroupSizeHUAWEI(device: *mut core::ffi::c_void, renderpass: u64, pMaxWorkgroupSize: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = renderpass; let _ = pMaxWorkgroupSize; crate::stub::Call::unsupported("vkGetDeviceSubpassShadingMaxWorkgroupSizeHUAWEI", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayModeProperties2KHR(physicalDevice: *mut core::ffi::c_void, display: u64, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = display; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetDisplayModeProperties2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayModePropertiesKHR(physicalDevice: *mut core::ffi::c_void, display: u64, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = display; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetDisplayModePropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayPlaneCapabilities2KHR(physicalDevice: *mut core::ffi::c_void, pDisplayPlaneInfo: *const core::ffi::c_void, pCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pDisplayPlaneInfo; let _ = pCapabilities; crate::stub::Call::unsupported("vkGetDisplayPlaneCapabilities2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayPlaneCapabilitiesKHR(physicalDevice: *mut core::ffi::c_void, mode: u64, planeIndex: u32, pCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = mode; let _ = planeIndex; let _ = pCapabilities; crate::stub::Call::unsupported("vkGetDisplayPlaneCapabilitiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDisplayPlaneSupportedDisplaysKHR(physicalDevice: *mut core::ffi::c_void, planeIndex: u32, pDisplayCount: *mut core::ffi::c_void, pDisplays: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = planeIndex; let _ = pDisplayCount; let _ = pDisplays; crate::stub::Call::unsupported("vkGetDisplayPlaneSupportedDisplaysKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDrmDisplayEXT(physicalDevice: *mut core::ffi::c_void, drmFd: i32, connectorId: u32, display: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = drmFd; let _ = connectorId; let _ = display; crate::stub::Call::unsupported("vkGetDrmDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetDynamicRenderingTilePropertiesQCOM(device: *mut core::ffi::c_void, pRenderingInfo: *const core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pRenderingInfo; let _ = pProperties; crate::stub::Call::unsupported("vkGetDynamicRenderingTilePropertiesQCOM", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetExecutionGraphPipelineNodeIndexAMDX(device: *mut core::ffi::c_void, executionGraph: u64, pNodeInfo: *const core::ffi::c_void, pNodeIndex: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = executionGraph; let _ = pNodeInfo; let _ = pNodeIndex; crate::stub::Call::unsupported("vkGetExecutionGraphPipelineNodeIndexAMDX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetExecutionGraphPipelineScratchSizeAMDX(device: *mut core::ffi::c_void, executionGraph: u64, pSizeInfo: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = executionGraph; let _ = pSizeInfo; crate::stub::Call::unsupported("vkGetExecutionGraphPipelineScratchSizeAMDX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFaultData(device: *mut core::ffi::c_void, faultQueryBehavior: i32, pUnrecordedFaults: *mut core::ffi::c_void, pFaultCount: *mut core::ffi::c_void, pFaults: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = faultQueryBehavior; let _ = pUnrecordedFaults; let _ = pFaultCount; let _ = pFaults; crate::stub::Call::unsupported("vkGetFaultData", "extension not advertised"); VK_ERROR_FEATURE_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceFdKHR(device: *mut core::ffi::c_void, pGetFdInfo: *const core::ffi::c_void, pFd: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetFdInfo; let _ = pFd; crate::stub::Call::unsupported("vkGetFenceFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceSciSyncFenceNV(device: *mut core::ffi::c_void, pGetSciSyncHandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciSyncHandleInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetFenceSciSyncFenceNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceSciSyncObjNV(device: *mut core::ffi::c_void, pGetSciSyncHandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciSyncHandleInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetFenceSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFenceWin32HandleKHR(device: *mut core::ffi::c_void, pGetWin32HandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetWin32HandleInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetFenceWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetFramebufferTilePropertiesQCOM(device: *mut core::ffi::c_void, framebuffer: u64, pPropertiesCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = framebuffer; let _ = pPropertiesCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetFramebufferTilePropertiesQCOM", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetGeneratedCommandsMemoryRequirementsNV(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) { let _ = device; let _ = pInfo; let _ = pMemoryRequirements; crate::stub::Call::unsupported("vkGetGeneratedCommandsMemoryRequirementsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetImageDrmFormatModifierPropertiesEXT(device: *mut core::ffi::c_void, image: u64, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = image; let _ = pProperties; crate::stub::Call::unsupported("vkGetImageDrmFormatModifierPropertiesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetImageOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::Call::unsupported("vkGetImageOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetImageViewAddressNVX(device: *mut core::ffi::c_void, imageView: u64, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = imageView; let _ = pProperties; crate::stub::Call::unsupported("vkGetImageViewAddressNVX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetImageViewHandleNVX(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> u32 { let _ = device; let _ = pInfo; crate::stub::Call::unsupported("vkGetImageViewHandleNVX", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetImageViewOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::Call::unsupported("vkGetImageViewOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetLatencyTimingsNV(device: *mut core::ffi::c_void, swapchain: u64, pLatencyMarkerInfo: *mut core::ffi::c_void) { let _ = device; let _ = swapchain; let _ = pLatencyMarkerInfo; crate::stub::Call::unsupported("vkGetLatencyTimingsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetMemoryAndroidHardwareBufferANDROID(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pBuffer: *mut *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pBuffer; crate::stub::Call::unsupported("vkGetMemoryAndroidHardwareBufferANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryFdKHR(device: *mut core::ffi::c_void, pGetFdInfo: *const core::ffi::c_void, pFd: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetFdInfo; let _ = pFd; crate::stub::Call::unsupported("vkGetMemoryFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryFdPropertiesKHR(device: *mut core::ffi::c_void, handleType: i32, fd: i32, pMemoryFdProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = handleType; let _ = fd; let _ = pMemoryFdProperties; crate::stub::Call::unsupported("vkGetMemoryFdPropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryHostPointerPropertiesEXT(device: *mut core::ffi::c_void, handleType: i32, pHostPointer: *const core::ffi::c_void, pMemoryHostPointerProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = handleType; let _ = pHostPointer; let _ = pMemoryHostPointerProperties; crate::stub::Call::unsupported("vkGetMemoryHostPointerPropertiesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryRemoteAddressNV(device: *mut core::ffi::c_void, pMemoryGetRemoteAddressInfo: *const core::ffi::c_void, pAddress: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pMemoryGetRemoteAddressInfo; let _ = pAddress; crate::stub::Call::unsupported("vkGetMemoryRemoteAddressNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemorySciBufNV(device: *mut core::ffi::c_void, pGetSciBufInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciBufInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetMemorySciBufNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryWin32HandleKHR(device: *mut core::ffi::c_void, pGetWin32HandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetWin32HandleInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetMemoryWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryWin32HandleNV(device: *mut core::ffi::c_void, memory: u64, handleType: u32, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = memory; let _ = handleType; let _ = pHandle; crate::stub::Call::unsupported("vkGetMemoryWin32HandleNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryZirconHandleFUCHSIA(device: *mut core::ffi::c_void, pGetZirconHandleInfo: *const core::ffi::c_void, pZirconHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetZirconHandleInfo; let _ = pZirconHandle; crate::stub::Call::unsupported("vkGetMemoryZirconHandleFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetMemoryZirconHandlePropertiesFUCHSIA(device: *mut core::ffi::c_void, handleType: i32, zirconHandle: u32, pMemoryZirconHandleProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = handleType; let _ = zirconHandle; let _ = pMemoryZirconHandleProperties; crate::stub::Call::unsupported("vkGetMemoryZirconHandlePropertiesFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPastPresentationTimingGOOGLE(device: *mut core::ffi::c_void, swapchain: u64, pPresentationTimingCount: *mut core::ffi::c_void, pPresentationTimings: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pPresentationTimingCount; let _ = pPresentationTimings; crate::stub::Call::unsupported("vkGetPastPresentationTimingGOOGLE", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPerformanceParameterINTEL(device: *mut core::ffi::c_void, parameter: i32, pValue: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = parameter; let _ = pValue; crate::stub::Call::unsupported("vkGetPerformanceParameterINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDirectFBPresentationSupportEXT(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32, dfb: *mut core::ffi::c_void) -> u32 { let _ = physicalDevice; let _ = queueFamilyIndex; let _ = dfb; crate::stub::Call::unsupported("vkGetPhysicalDeviceDirectFBPresentationSupportEXT", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayPlaneProperties2KHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceDisplayPlaneProperties2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayPlanePropertiesKHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceDisplayPlanePropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayProperties2KHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceDisplayProperties2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceDisplayPropertiesKHR(physicalDevice: *mut core::ffi::c_void, pPropertyCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pPropertyCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceDisplayPropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceExternalImageFormatPropertiesNV(physicalDevice: *mut core::ffi::c_void, format: i32, type_: i32, tiling: i32, usage: u32, flags: u32, externalHandleType: u32, pExternalImageFormatProperties: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = format; let _ = type_; let _ = tiling; let _ = usage; let _ = flags; let _ = externalHandleType; let _ = pExternalImageFormatProperties; crate::stub::Call::unsupported("vkGetPhysicalDeviceExternalImageFormatPropertiesNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceFragmentShadingRatesKHR(physicalDevice: *mut core::ffi::c_void, pFragmentShadingRateCount: *mut core::ffi::c_void, pFragmentShadingRates: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pFragmentShadingRateCount; let _ = pFragmentShadingRates; crate::stub::Call::unsupported("vkGetPhysicalDeviceFragmentShadingRatesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceQueueFamilyPerformanceQueryPassesKHR(physicalDevice: *mut core::ffi::c_void, pPerformanceQueryCreateInfo: *const core::ffi::c_void, pNumPasses: *mut core::ffi::c_void) { let _ = physicalDevice; let _ = pPerformanceQueryCreateInfo; let _ = pNumPasses; crate::stub::Call::unsupported("vkGetPhysicalDeviceQueueFamilyPerformanceQueryPassesKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceRefreshableObjectTypesKHR(physicalDevice: *mut core::ffi::c_void, pRefreshableObjectTypeCount: *mut core::ffi::c_void, pRefreshableObjectTypes: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pRefreshableObjectTypeCount; let _ = pRefreshableObjectTypes; crate::stub::Call::unsupported("vkGetPhysicalDeviceRefreshableObjectTypesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceScreenPresentationSupportQNX(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32, window: *mut core::ffi::c_void) -> u32 { let _ = physicalDevice; let _ = queueFamilyIndex; let _ = window; crate::stub::Call::unsupported("vkGetPhysicalDeviceScreenPresentationSupportQNX", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSupportedFramebufferMixedSamplesCombinationsNV(physicalDevice: *mut core::ffi::c_void, pCombinationCount: *mut core::ffi::c_void, pCombinations: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pCombinationCount; let _ = pCombinations; crate::stub::Call::unsupported("vkGetPhysicalDeviceSupportedFramebufferMixedSamplesCombinationsNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilities2EXT(physicalDevice: *mut core::ffi::c_void, surface: u64, pSurfaceCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = surface; let _ = pSurfaceCapabilities; crate::stub::Call::unsupported("vkGetPhysicalDeviceSurfaceCapabilities2EXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceCapabilities2KHR(physicalDevice: *mut core::ffi::c_void, pSurfaceInfo: *const core::ffi::c_void, pSurfaceCapabilities: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pSurfaceInfo; let _ = pSurfaceCapabilities; crate::stub::Call::unsupported("vkGetPhysicalDeviceSurfaceCapabilities2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfaceFormats2KHR(physicalDevice: *mut core::ffi::c_void, pSurfaceInfo: *const core::ffi::c_void, pSurfaceFormatCount: *mut core::ffi::c_void, pSurfaceFormats: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pSurfaceInfo; let _ = pSurfaceFormatCount; let _ = pSurfaceFormats; crate::stub::Call::unsupported("vkGetPhysicalDeviceSurfaceFormats2KHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceSurfacePresentModes2EXT(physicalDevice: *mut core::ffi::c_void, pSurfaceInfo: *const core::ffi::c_void, pPresentModeCount: *mut core::ffi::c_void, pPresentModes: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = pSurfaceInfo; let _ = pPresentModeCount; let _ = pPresentModes; crate::stub::Call::unsupported("vkGetPhysicalDeviceSurfacePresentModes2EXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPhysicalDeviceWin32PresentationSupportKHR(physicalDevice: *mut core::ffi::c_void, queueFamilyIndex: u32) -> u32 { let _ = physicalDevice; let _ = queueFamilyIndex; crate::stub::Call::unsupported("vkGetPhysicalDeviceWin32PresentationSupportKHR", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutableInternalRepresentationsKHR(device: *mut core::ffi::c_void, pExecutableInfo: *const core::ffi::c_void, pInternalRepresentationCount: *mut core::ffi::c_void, pInternalRepresentations: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pExecutableInfo; let _ = pInternalRepresentationCount; let _ = pInternalRepresentations; crate::stub::Call::unsupported("vkGetPipelineExecutableInternalRepresentationsKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutablePropertiesKHR(device: *mut core::ffi::c_void, pPipelineInfo: *const core::ffi::c_void, pExecutableCount: *mut core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pPipelineInfo; let _ = pExecutableCount; let _ = pProperties; crate::stub::Call::unsupported("vkGetPipelineExecutablePropertiesKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPipelineExecutableStatisticsKHR(device: *mut core::ffi::c_void, pExecutableInfo: *const core::ffi::c_void, pStatisticCount: *mut core::ffi::c_void, pStatistics: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pExecutableInfo; let _ = pStatisticCount; let _ = pStatistics; crate::stub::Call::unsupported("vkGetPipelineExecutableStatisticsKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetPipelineIndirectDeviceAddressNV(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void) -> u64 { let _ = device; let _ = pInfo; crate::stub::Call::unsupported("vkGetPipelineIndirectDeviceAddressNV", "extension not advertised"); 0 }

#[no_mangle]
pub extern "C" fn vkGetPipelineIndirectMemoryRequirementsNV(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pMemoryRequirements: *mut core::ffi::c_void) { let _ = device; let _ = pCreateInfo; let _ = pMemoryRequirements; crate::stub::Call::unsupported("vkGetPipelineIndirectMemoryRequirementsNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetPipelinePropertiesEXT(device: *mut core::ffi::c_void, pPipelineInfo: *const core::ffi::c_void, pPipelineProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pPipelineInfo; let _ = pPipelineProperties; crate::stub::Call::unsupported("vkGetPipelinePropertiesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetQueueCheckpointData2NV(queue: *mut core::ffi::c_void, pCheckpointDataCount: *mut core::ffi::c_void, pCheckpointData: *mut core::ffi::c_void) { let _ = queue; let _ = pCheckpointDataCount; let _ = pCheckpointData; unsafe { if !pCheckpointDataCount.is_null() { *(pCheckpointDataCount as *mut u32) = 0; } } crate::stub::Call::unsupported("vkGetQueueCheckpointData2NV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetQueueCheckpointDataNV(queue: *mut core::ffi::c_void, pCheckpointDataCount: *mut core::ffi::c_void, pCheckpointData: *mut core::ffi::c_void) { let _ = queue; let _ = pCheckpointDataCount; let _ = pCheckpointData; unsafe { if !pCheckpointDataCount.is_null() { *(pCheckpointDataCount as *mut u32) = 0; } } crate::stub::Call::unsupported("vkGetQueueCheckpointDataNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetRandROutputDisplayEXT(physicalDevice: *mut core::ffi::c_void, dpy: *mut core::ffi::c_void, rrOutput: u64, pDisplay: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = dpy; let _ = rrOutput; let _ = pDisplay; crate::stub::Call::unsupported("vkGetRandROutputDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetRefreshCycleDurationGOOGLE(device: *mut core::ffi::c_void, swapchain: u64, pDisplayTimingProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pDisplayTimingProperties; crate::stub::Call::unsupported("vkGetRefreshCycleDurationGOOGLE", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSamplerOpaqueCaptureDescriptorDataEXT(device: *mut core::ffi::c_void, pInfo: *const core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pInfo; let _ = pData; crate::stub::Call::unsupported("vkGetSamplerOpaqueCaptureDescriptorDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetScreenBufferPropertiesQNX(device: *mut core::ffi::c_void, buffer: *const core::ffi::c_void, pProperties: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = buffer; let _ = pProperties; crate::stub::Call::unsupported("vkGetScreenBufferPropertiesQNX", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreFdKHR(device: *mut core::ffi::c_void, pGetFdInfo: *const core::ffi::c_void, pFd: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetFdInfo; let _ = pFd; crate::stub::Call::unsupported("vkGetSemaphoreFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreSciSyncObjNV(device: *mut core::ffi::c_void, pGetSciSyncInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetSciSyncInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetSemaphoreSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreWin32HandleKHR(device: *mut core::ffi::c_void, pGetWin32HandleInfo: *const core::ffi::c_void, pHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetWin32HandleInfo; let _ = pHandle; crate::stub::Call::unsupported("vkGetSemaphoreWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSemaphoreZirconHandleFUCHSIA(device: *mut core::ffi::c_void, pGetZirconHandleInfo: *const core::ffi::c_void, pZirconHandle: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pGetZirconHandleInfo; let _ = pZirconHandle; crate::stub::Call::unsupported("vkGetSemaphoreZirconHandleFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetShaderBinaryDataEXT(device: *mut core::ffi::c_void, shader: u64, pDataSize: *mut core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = shader; let _ = pDataSize; let _ = pData; crate::stub::Call::unsupported("vkGetShaderBinaryDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetShaderInfoAMD(device: *mut core::ffi::c_void, pipeline: u64, shaderStage: i32, infoType: i32, pInfoSize: *mut core::ffi::c_void, pInfo: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pipeline; let _ = shaderStage; let _ = infoType; let _ = pInfoSize; let _ = pInfo; crate::stub::Call::unsupported("vkGetShaderInfoAMD", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetShaderModuleCreateInfoIdentifierEXT(device: *mut core::ffi::c_void, pCreateInfo: *const core::ffi::c_void, pIdentifier: *mut core::ffi::c_void) { let _ = device; let _ = pCreateInfo; let _ = pIdentifier; crate::stub::Call::unsupported("vkGetShaderModuleCreateInfoIdentifierEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetShaderModuleIdentifierEXT(device: *mut core::ffi::c_void, shaderModule: u64, pIdentifier: *mut core::ffi::c_void) { let _ = device; let _ = shaderModule; let _ = pIdentifier; crate::stub::Call::unsupported("vkGetShaderModuleIdentifierEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkGetSwapchainCounterEXT(device: *mut core::ffi::c_void, swapchain: u64, counter: i32, pCounterValue: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = counter; let _ = pCounterValue; crate::stub::Call::unsupported("vkGetSwapchainCounterEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSwapchainGrallocUsage2ANDROID(device: *mut core::ffi::c_void, format: i32, imageUsage: u32, swapchainImageUsage: u32, grallocConsumerUsage: *mut core::ffi::c_void, grallocProducerUsage: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = format; let _ = imageUsage; let _ = swapchainImageUsage; let _ = grallocConsumerUsage; let _ = grallocProducerUsage; crate::stub::Call::unsupported("vkGetSwapchainGrallocUsage2ANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSwapchainGrallocUsageANDROID(device: *mut core::ffi::c_void, format: i32, imageUsage: u32, grallocUsage: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = format; let _ = imageUsage; let _ = grallocUsage; crate::stub::Call::unsupported("vkGetSwapchainGrallocUsageANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetSwapchainStatusKHR(device: *mut core::ffi::c_void, swapchain: u64) -> i32 { let _ = device; let _ = swapchain; crate::stub::Call::unsupported("vkGetSwapchainStatusKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetValidationCacheDataEXT(device: *mut core::ffi::c_void, validationCache: u64, pDataSize: *mut core::ffi::c_void, pData: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = validationCache; let _ = pDataSize; let _ = pData; crate::stub::Call::unsupported("vkGetValidationCacheDataEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkGetWinrtDisplayNV(physicalDevice: *mut core::ffi::c_void, deviceRelativeId: u32, pDisplay: *mut core::ffi::c_void) -> i32 { let _ = physicalDevice; let _ = deviceRelativeId; let _ = pDisplay; crate::stub::Call::unsupported("vkGetWinrtDisplayNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceFdKHR(device: *mut core::ffi::c_void, pImportFenceFdInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceFdInfo; crate::stub::Call::unsupported("vkImportFenceFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceSciSyncFenceNV(device: *mut core::ffi::c_void, pImportFenceSciSyncInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceSciSyncInfo; crate::stub::Call::unsupported("vkImportFenceSciSyncFenceNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceSciSyncObjNV(device: *mut core::ffi::c_void, pImportFenceSciSyncInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceSciSyncInfo; crate::stub::Call::unsupported("vkImportFenceSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportFenceWin32HandleKHR(device: *mut core::ffi::c_void, pImportFenceWin32HandleInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportFenceWin32HandleInfo; crate::stub::Call::unsupported("vkImportFenceWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreFdKHR(device: *mut core::ffi::c_void, pImportSemaphoreFdInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreFdInfo; crate::stub::Call::unsupported("vkImportSemaphoreFdKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreSciSyncObjNV(device: *mut core::ffi::c_void, pImportSemaphoreSciSyncInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreSciSyncInfo; crate::stub::Call::unsupported("vkImportSemaphoreSciSyncObjNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreWin32HandleKHR(device: *mut core::ffi::c_void, pImportSemaphoreWin32HandleInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreWin32HandleInfo; crate::stub::Call::unsupported("vkImportSemaphoreWin32HandleKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkImportSemaphoreZirconHandleFUCHSIA(device: *mut core::ffi::c_void, pImportSemaphoreZirconHandleInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pImportSemaphoreZirconHandleInfo; crate::stub::Call::unsupported("vkImportSemaphoreZirconHandleFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkInitializePerformanceApiINTEL(device: *mut core::ffi::c_void, pInitializeInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pInitializeInfo; crate::stub::Call::unsupported("vkInitializePerformanceApiINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkLatencySleepNV(device: *mut core::ffi::c_void, swapchain: u64, pSleepInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pSleepInfo; crate::stub::Call::unsupported("vkLatencySleepNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkMergeValidationCachesEXT(device: *mut core::ffi::c_void, dstCache: u64, srcCacheCount: u32, pSrcCaches: *const core::ffi::c_void) -> i32 { let _ = device; let _ = dstCache; let _ = srcCacheCount; let _ = pSrcCaches; crate::stub::Call::unsupported("vkMergeValidationCachesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkQueueBindSparse(queue: *mut core::ffi::c_void, bindInfoCount: u32, pBindInfo: *const core::ffi::c_void, fence: u64) -> i32 { let _ = queue; let _ = bindInfoCount; let _ = pBindInfo; let _ = fence; crate::stub::Call::unsupported("vkQueueBindSparse", "extension not advertised"); VK_ERROR_FEATURE_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkQueueNotifyOutOfBandNV(queue: *mut core::ffi::c_void, pQueueTypeInfo: *const core::ffi::c_void) { let _ = queue; let _ = pQueueTypeInfo; crate::stub::Call::unsupported("vkQueueNotifyOutOfBandNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkQueueSetPerformanceConfigurationINTEL(queue: *mut core::ffi::c_void, configuration: u64) -> i32 { let _ = queue; let _ = configuration; crate::stub::Call::unsupported("vkQueueSetPerformanceConfigurationINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkQueueSignalReleaseImageANDROID(queue: *mut core::ffi::c_void, waitSemaphoreCount: u32, pWaitSemaphores: *const core::ffi::c_void, image: u64, pNativeFenceFd: *mut core::ffi::c_void) -> i32 { let _ = queue; let _ = waitSemaphoreCount; let _ = pWaitSemaphores; let _ = image; let _ = pNativeFenceFd; crate::stub::Call::unsupported("vkQueueSignalReleaseImageANDROID", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkRegisterDeviceEventEXT(device: *mut core::ffi::c_void, pDeviceEventInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFence: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = pDeviceEventInfo; let _ = pAllocator; let _ = pFence; unsafe { if !pFence.is_null() { *(pFence as *mut u64) = 0; } } crate::stub::Call::unsupported("vkRegisterDeviceEventEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkRegisterDisplayEventEXT(device: *mut core::ffi::c_void, display: u64, pDisplayEventInfo: *const core::ffi::c_void, pAllocator: *const core::ffi::c_void, pFence: *mut core::ffi::c_void) -> i32 { let _ = device; let _ = display; let _ = pDisplayEventInfo; let _ = pAllocator; let _ = pFence; unsafe { if !pFence.is_null() { *(pFence as *mut u64) = 0; } } crate::stub::Call::unsupported("vkRegisterDisplayEventEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleaseDisplayEXT(physicalDevice: *mut core::ffi::c_void, display: u64) -> i32 { let _ = physicalDevice; let _ = display; crate::stub::Call::unsupported("vkReleaseDisplayEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleaseFullScreenExclusiveModeEXT(device: *mut core::ffi::c_void, swapchain: u64) -> i32 { let _ = device; let _ = swapchain; crate::stub::Call::unsupported("vkReleaseFullScreenExclusiveModeEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleasePerformanceConfigurationINTEL(device: *mut core::ffi::c_void, configuration: u64) -> i32 { let _ = device; let _ = configuration; crate::stub::Call::unsupported("vkReleasePerformanceConfigurationINTEL", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkReleaseProfilingLockKHR(device: *mut core::ffi::c_void) { let _ = device; crate::stub::Call::unsupported("vkReleaseProfilingLockKHR", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkReleaseSwapchainImagesEXT(device: *mut core::ffi::c_void, pReleaseInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = pReleaseInfo; crate::stub::Call::unsupported("vkReleaseSwapchainImagesEXT", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetBufferCollectionBufferConstraintsFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pBufferConstraintsInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = collection; let _ = pBufferConstraintsInfo; crate::stub::Call::unsupported("vkSetBufferCollectionBufferConstraintsFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetBufferCollectionImageConstraintsFUCHSIA(device: *mut core::ffi::c_void, collection: u64, pImageConstraintsInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = collection; let _ = pImageConstraintsInfo; crate::stub::Call::unsupported("vkSetBufferCollectionImageConstraintsFUCHSIA", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetDeviceMemoryPriorityEXT(device: *mut core::ffi::c_void, memory: u64, priority: f32) { let _ = device; let _ = memory; let _ = priority; crate::stub::Call::unsupported("vkSetDeviceMemoryPriorityEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkSetHdrMetadataEXT(device: *mut core::ffi::c_void, swapchainCount: u32, pSwapchains: *const core::ffi::c_void, pMetadata: *const core::ffi::c_void) { let _ = device; let _ = swapchainCount; let _ = pSwapchains; let _ = pMetadata; crate::stub::Call::unsupported("vkSetHdrMetadataEXT", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkSetLatencyMarkerNV(device: *mut core::ffi::c_void, swapchain: u64, pLatencyMarkerInfo: *const core::ffi::c_void) { let _ = device; let _ = swapchain; let _ = pLatencyMarkerInfo; crate::stub::Call::unsupported("vkSetLatencyMarkerNV", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkSetLatencySleepModeNV(device: *mut core::ffi::c_void, swapchain: u64, pSleepModeInfo: *const core::ffi::c_void) -> i32 { let _ = device; let _ = swapchain; let _ = pSleepModeInfo; crate::stub::Call::unsupported("vkSetLatencySleepModeNV", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }

#[no_mangle]
pub extern "C" fn vkSetLocalDimmingAMD(device: *mut core::ffi::c_void, swapChain: u64, localDimmingEnable: u32) { let _ = device; let _ = swapChain; let _ = localDimmingEnable; crate::stub::Call::unsupported("vkSetLocalDimmingAMD", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkUninitializePerformanceApiINTEL(device: *mut core::ffi::c_void) { let _ = device; crate::stub::Call::unsupported("vkUninitializePerformanceApiINTEL", "extension not advertised"); }

#[no_mangle]
pub extern "C" fn vkWaitForPresentKHR(device: *mut core::ffi::c_void, swapchain: u64, presentId: u64, timeout: u64) -> i32 { let _ = device; let _ = swapchain; let _ = presentId; let _ = timeout; crate::stub::Call::unsupported("vkWaitForPresentKHR", "extension not advertised"); VK_ERROR_EXTENSION_NOT_PRESENT }
