//! Shim dispatch, ABI census, and implemented-command behavior.

use super::*;

#[test]
fn surface_is_complete_and_matches_the_census() {
    assert_eq!(
        VK_ENTRYPOINTS, 712,
        "Vulkan command surface drifted from the golden 712"
    );
    assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
    assert_eq!(DISPATCH_NAMES.len(), 712, "dispatch census drifted");
}

#[test]
fn every_implemented_command_resolves() {
    // Spot-check that the hand-written bring-up + compute commands resolve through the same
    // name→address table the loader uses.
    for name in [
        "vkGetInstanceProcAddr",
        "vkCreateInstance",
        "vkEnumeratePhysicalDevices",
        "vkGetPhysicalDeviceProperties",
        "vkCreateDevice",
        "vkCreateBuffer",
        "vkCreateShaderModule",
        "vkCreateComputePipelines",
        "vkQueueSubmit",
        "vkWaitForFences",
        // newly hand-written families resolve through the same table:
        "vkTrimCommandPool",
        "vkGetDeviceBufferMemoryRequirements",
        "vkSetPrivateData",
        "vkCreateSamplerYcbcrConversion",
        "vkCmdTraceRaysKHR",
        "vkCreateRenderPass2",
    ] {
        assert!(dispatch_addr(name).is_some(), "{name} does not resolve");
    }
}

// ---- hand-written maintenance / host-copy / not-supported bodies ------------------------------

use crate::types::*;
use core::ffi::c_void;

/// The `vk*` state is a process-global singleton and `vkCreateDevice` replaces the logical device
/// wholesale, so tests that create a device + then rely on device-owned objects (buffers, command
/// buffers) persisting across calls must not run concurrently with another device-creating test.
/// Every such test takes this lock. (Poison-tolerant: a panicked test still yields the guard.)
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub(super) fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Create a device, a command pool, one command buffer, and put it into the `Recording` state.
/// Returns `(dispatchable VkCommandBuffer, its u64 handle)`. Caller must hold [`test_guard`].
pub(super) fn recording_command_buffer() -> (*mut c_void, u64) {
    let mut dev: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut dev
        ),
        VK_SUCCESS
    );
    let mut pool: u64 = 0;
    assert_eq!(
        crate::compute::vkCreateCommandPool(dev, core::ptr::null(), core::ptr::null(), &mut pool),
        VK_SUCCESS
    );
    let ai = VkCommandBufferAllocateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        command_pool: pool,
        level: 0,
        command_buffer_count: 1,
    };
    let mut cb: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        crate::compute::vkAllocateCommandBuffers(dev, &ai as *const _ as *const c_void, &mut cb),
        VK_SUCCESS
    );
    assert_eq!(
        crate::compute::vkBeginCommandBuffer(cb, core::ptr::null()),
        VK_SUCCESS
    );
    let handle = unsafe { *Dispatchable::<u64>::inner(cb).unwrap() };
    (cb, handle)
}

#[test]
fn device_buffer_memory_requirements_reports_size_and_alias_matches_base() {
    let ci = VkBufferCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        size: 4096,
        usage: 0,
        sharing_mode: 0,
        queue_family_index_count: 0,
        p_queue_family_indices: core::ptr::null(),
    };
    let info = VkDeviceBufferMemoryRequirements {
        s_type: 0,
        p_next: core::ptr::null(),
        p_create_info: &ci,
    };
    let mut base = VkMemoryRequirements2 {
        s_type: 0,
        p_next: core::ptr::null_mut(),
        memory_requirements: VkMemoryRequirements {
            size: 0,
            alignment: 0,
            memory_type_bits: 0,
        },
    };
    let mut khr = VkMemoryRequirements2 {
        s_type: 0,
        p_next: core::ptr::null_mut(),
        memory_requirements: VkMemoryRequirements {
            size: 0,
            alignment: 0,
            memory_type_bits: 0,
        },
    };
    crate::maintenance::vkGetDeviceBufferMemoryRequirements(
        core::ptr::null_mut(),
        &info as *const _ as *const c_void,
        &mut base as *mut _ as *mut c_void,
    );
    crate::maintenance::vkGetDeviceBufferMemoryRequirementsKHR(
        core::ptr::null_mut(),
        &info as *const _ as *const c_void,
        &mut khr as *mut _ as *mut c_void,
    );
    assert_eq!(base.memory_requirements.size, 4096);
    assert_eq!(base.memory_requirements.alignment, 256);
    // the KHR alias produces byte-identical output to the core body.
    assert_eq!(khr.memory_requirements.size, base.memory_requirements.size);
    assert_eq!(
        khr.memory_requirements.memory_type_bits,
        base.memory_requirements.memory_type_bits
    );
    // memoryTypeBits exposes EVERY advertised memory type (all our memory is host RAM, so any
    // resource can be backed by any type). This is what lets gpu-alloc pick a type per usage.
    let want_bits = hl_vulkan::PhysicalDeviceDesc::hl_default().all_memory_type_bits();
    assert_eq!(base.memory_requirements.memory_type_bits, want_bits);
    assert!(
        want_bits > 1,
        "must expose more than the single legacy type (index 0)"
    );
}

/// The advertised `VkPhysicalDeviceMemoryProperties` are the STANDARD software-Vulkan set (mirrors
/// lavapipe): valid heap indices, at least one HOST_VISIBLE|HOST_COHERENT type, a mappable type
/// exists, and every reportable `memoryTypeBits` bit maps to a real type. A regression here is what
/// made wgpu-hal's gpu-alloc mis-serve Zed's allocations.
#[test]
fn advertised_memory_properties_are_the_standard_set() {
    const DEVICE_LOCAL: VkFlags = 0x1;
    const HOST_VISIBLE: VkFlags = 0x2;
    const HOST_COHERENT: VkFlags = 0x4;
    const HOST_CACHED: VkFlags = 0x8;

    let mut mp = VkPhysicalDeviceMemoryProperties {
        memory_type_count: 0,
        memory_types: [VkMemoryType::default(); VK_MAX_MEMORY_TYPES],
        memory_heap_count: 0,
        memory_heaps: [VkMemoryHeap::default(); VK_MAX_MEMORY_HEAPS],
    };
    crate::instance::vkGetPhysicalDeviceMemoryProperties(
        core::ptr::null_mut(),
        &mut mp as *mut _ as *mut c_void,
    );

    // At least one heap, at least one non-empty heap, and a DEVICE_LOCAL heap.
    let nheaps = mp.memory_heap_count as usize;
    assert!(nheaps >= 1 && nheaps <= VK_MAX_MEMORY_HEAPS);
    assert!(
        mp.memory_heaps[..nheaps].iter().all(|h| h.size > 0),
        "every heap must have a real size"
    );
    assert!(
        mp.memory_heaps[..nheaps]
            .iter()
            .any(|h| h.flags & DEVICE_LOCAL != 0),
        "a DEVICE_LOCAL heap must be advertised"
    );

    // The standard multi-type layout: more than one type, every type points at a valid heap.
    let ntypes = mp.memory_type_count as usize;
    assert!(
        ntypes >= 2 && ntypes <= VK_MAX_MEMORY_TYPES,
        "must advertise the standard multi-type set"
    );
    for t in &mp.memory_types[..ntypes] {
        assert!(
            (t.heap_index as usize) < nheaps,
            "memory type references an out-of-range heap"
        );
    }

    // A plain HOST_VISIBLE|HOST_COHERENT upload type exists (what gpu-alloc wants for UPLOAD).
    assert!(
        mp.memory_types[..ntypes]
            .iter()
            .any(|t| t.property_flags & (HOST_VISIBLE | HOST_COHERENT)
                == (HOST_VISIBLE | HOST_COHERENT)),
        "a HOST_VISIBLE|HOST_COHERENT type must exist"
    );
    // A mappable (HOST_VISIBLE) type exists — every HOST_VISIBLE type IS mappable via vkMapMemory.
    assert!(
        mp.memory_types[..ntypes]
            .iter()
            .any(|t| t.property_flags & HOST_VISIBLE != 0),
        "a mappable HOST_VISIBLE type must exist"
    );
    // A HOST_CACHED type exists for readback/download.
    assert!(
        mp.memory_types[..ntypes]
            .iter()
            .any(|t| t.property_flags & HOST_CACHED != 0),
        "a HOST_CACHED type must exist for downloads"
    );
    // A DEVICE_LOCAL type exists for GPU-only resources.
    assert!(
        mp.memory_types[..ntypes]
            .iter()
            .any(|t| t.property_flags & DEVICE_LOCAL != 0),
        "a DEVICE_LOCAL type must exist"
    );

    // Every bit our resources report in memoryTypeBits indexes a real advertised type.
    let bits = hl_vulkan::PhysicalDeviceDesc::hl_default().all_memory_type_bits();
    assert_eq!(
        bits,
        (1u32 << ntypes) - 1,
        "memoryTypeBits must cover exactly the advertised types"
    );

    // ...2 delegates to the 1.0 fill: byte-identical memory_properties.
    let mut mp2 = VkPhysicalDeviceMemoryProperties2 {
        s_type: 0,
        p_next: core::ptr::null_mut(),
        memory_properties: VkPhysicalDeviceMemoryProperties {
            memory_type_count: 0,
            memory_types: [VkMemoryType::default(); VK_MAX_MEMORY_TYPES],
            memory_heap_count: 0,
            memory_heaps: [VkMemoryHeap::default(); VK_MAX_MEMORY_HEAPS],
        },
    };
    crate::instance::vkGetPhysicalDeviceMemoryProperties2(
        core::ptr::null_mut(),
        &mut mp2 as *mut _ as *mut c_void,
    );
    assert_eq!(
        mp2.memory_properties.memory_type_count,
        mp.memory_type_count
    );
    assert_eq!(
        mp2.memory_properties.memory_heap_count,
        mp.memory_heap_count
    );
    for i in 0..ntypes {
        assert_eq!(
            mp2.memory_properties.memory_types[i].property_flags,
            mp.memory_types[i].property_flags
        );
        assert_eq!(
            mp2.memory_properties.memory_types[i].heap_index,
            mp.memory_types[i].heap_index
        );
    }
}

#[test]
fn descriptor_set_layout_support_reports_supported() {
    let mut sup = VkDescriptorSetLayoutSupport {
        s_type: 0,
        p_next: core::ptr::null_mut(),
        supported: 0,
    };
    crate::maintenance::vkGetDescriptorSetLayoutSupport(
        core::ptr::null_mut(),
        core::ptr::null(),
        &mut sup as *mut _ as *mut c_void,
    );
    assert_eq!(sup.supported, VK_TRUE);
}

#[test]
fn host_image_copy_is_honestly_unsupported() {
    let _g = test_guard();
    // A device must exist (created below); a modeled `hostImageCopy` op returns the truthful error.
    let mut dev: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut dev
        ),
        VK_SUCCESS
    );
    let dummy = [0u8; 64];
    let r = crate::hostcopy::vkCopyMemoryToImage(dev, dummy.as_ptr() as *const c_void);
    assert_eq!(r, VK_ERROR_FEATURE_NOT_PRESENT);
    // the EXT alias matches the core body.
    assert_eq!(
        crate::hostcopy::vkCopyMemoryToImageEXT(dev, dummy.as_ptr() as *const c_void),
        r
    );
}

#[test]
fn private_data_round_trips_and_ycbcr_conversion_creates() {
    let _g = test_guard();
    let mut dev: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut dev
        ),
        VK_SUCCESS
    );
    // private data: create a slot, store a value under an (objectType, handle), read it back.
    let mut slot: u64 = 0;
    assert_eq!(
        crate::maintenance::vkCreatePrivateDataSlot(
            dev,
            core::ptr::null(),
            core::ptr::null(),
            &mut slot
        ),
        VK_SUCCESS
    );
    assert_ne!(slot, 0);
    assert_eq!(
        crate::maintenance::vkSetPrivateData(dev, 9, 0xABCD, slot, 0xDEAD_BEEF),
        VK_SUCCESS
    );
    let mut got: u64 = 0;
    crate::maintenance::vkGetPrivateData(dev, 9, 0xABCD, slot, &mut got);
    assert_eq!(got, 0xDEAD_BEEF);
    // an unset key reads back 0 (the spec default).
    let mut zero: u64 = 123;
    crate::maintenance::vkGetPrivateData(dev, 9, 0x0001, slot, &mut zero);
    assert_eq!(zero, 0);

    // ycbcr conversion: a non-null create info mints a live handle.
    let ycbcr_ci = [0u8; 64];
    let mut conv: u64 = 0;
    assert_eq!(
        crate::maintenance::vkCreateSamplerYcbcrConversion(
            dev,
            ycbcr_ci.as_ptr() as *const c_void,
            core::ptr::null(),
            &mut conv,
        ),
        VK_SUCCESS
    );
    assert_ne!(conv, 0);
    crate::maintenance::vkDestroySamplerYcbcrConversion(dev, conv, core::ptr::null());
}

#[test]
fn ray_tracing_family_returns_extension_not_present() {
    // A wholesale-unmodeled extension command validates + returns the truthful, non-faked error.
    let mut pipe: u64 = 12345;
    let r = crate::unsupported::vkCreateRayTracingPipelinesKHR(
        core::ptr::null_mut(),
        0,
        0,
        0,
        core::ptr::null(),
        core::ptr::null(),
        &mut pipe as *mut u64 as *mut c_void,
    );
    assert_eq!(r, -7); // VK_ERROR_EXTENSION_NOT_PRESENT
}

mod commands;
