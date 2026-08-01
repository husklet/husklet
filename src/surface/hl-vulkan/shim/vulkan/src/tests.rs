//! Shim dispatch, ABI census, and implemented-command behavior.

use super::*;

#[test]
fn surface_is_complete_and_matches_the_census() {
    assert_eq!(
        VK_ENTRYPOINTS, 712,
        "Vulkan command surface drifted from the golden 712"
    );
    assert_eq!(
        GENERATED_STUBS + LOWERED_ENTRYPOINTS + REFUSED_ENTRYPOINTS,
        TOTAL_ENTRYPOINTS
    );
    assert_eq!(DISPATCH_NAMES.len(), 712, "dispatch census drifted");
    // The three classes are disjoint: a refusal must never also be counted as a lowering.
    assert_eq!(
        LOWERED_NAMES
            .iter()
            .filter(|name| REFUSED_NAMES.contains(name))
            .collect::<Vec<_>>(),
        Vec::<&&str>::new()
    );
}

/// Every command core Vulkan mandates at or below the version this driver advertises must PERFORM its
/// operation. A refusal or a generated stub in that set is a capability lie the version number makes —
/// and for a `void` command it cannot even be reported to the caller, which is exactly how the core-1.4
/// push-descriptor family stayed silent. Data comes from `registry/vk_core_mandate.manifest` (Khronos
/// registry), so raising `HL_API_VERSION` past what is implemented fails here rather than in a client.
#[test]
fn every_core_mandated_command_at_the_advertised_version_is_lowered() {
    let unmet: Vec<&str> = CORE_MANDATE
        .iter()
        .filter(|(version, _)| *version <= HL_API_VERSION)
        .map(|(_, name)| *name)
        .filter(|name| !LOWERED_NAMES.contains(name))
        .collect();

    assert_eq!(unmet, Vec::<&str>::new());
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
        "must expose more than the single core-1.0 type (index 0)"
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

/// `VkApplicationInfo::apiVersion` states what the APPLICATION was written against, so a request above
/// the advertised version is clamped, not rejected: the app gets its instance and reads the real version
/// off the physical device. This previously returned `VK_ERROR_INCOMPATIBLE_DRIVER`, which is what made
/// the advertised version impossible to lower without rejecting Dawn.
#[test]
fn instance_clamps_an_api_request_above_the_advertised_version() {
    let _g = test_guard();
    let application = VkApplicationInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        p_application_name: core::ptr::null(),
        application_version: 0,
        p_engine_name: core::ptr::null(),
        engine_version: 0,
        api_version: HL_API_VERSION + (1 << 12),
    };
    let create = VkInstanceCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        p_application_info: &application,
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: core::ptr::null(),
    };
    let mut output = core::ptr::null_mut();

    assert_eq!(
        crate::instance::vkCreateInstance(
            &create as *const _ as *const c_void,
            core::ptr::null(),
            &mut output,
        ),
        VK_SUCCESS
    );
    assert!(!output.is_null());
    assert_eq!(
        crate::state::StateStore::with(|state| {
            state.instance.as_ref().unwrap().app_api_version
        }),
        HL_API_VERSION
    );
    let mut properties = [0u8; core::mem::size_of::<VkPhysicalDeviceProperties>()];
    crate::instance::vkGetPhysicalDeviceProperties(
        core::ptr::null_mut(),
        properties.as_mut_ptr() as *mut c_void,
    );
    let reported = unsafe { &*(properties.as_ptr() as *const VkPhysicalDeviceProperties) };
    assert_eq!(reported.api_version, HL_API_VERSION);
    crate::instance::vkDestroyInstance(output, core::ptr::null());
}

/// An implementation advertising Vulkan 1.1 or later must never answer `vkCreateInstance` with
/// `VK_ERROR_INCOMPATIBLE_DRIVER` — that result belongs to the 1.0 era, before an application could ask
/// `vkEnumerateInstanceVersion` first. A nonzero `variant` names a different API (Vulkan SC), but choosing
/// between implementations is the loader's job and is settled before the call reaches this ICD, so
/// refusing it here only rejected an application this driver could serve. The recorded version is this
/// driver's own, which is what the physical device reports for the caller to gate on.
#[test]
fn instance_accepts_a_foreign_api_variant_and_records_its_own_version() {
    let _g = test_guard();
    crate::state::StateStore::with(|state| {
        state.instance = Some(hl_vulkan::Instance::new(make_api_version(0, 1, 0, 0)));
    });
    let application = VkApplicationInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        p_application_name: core::ptr::null(),
        application_version: 0,
        p_engine_name: core::ptr::null(),
        engine_version: 0,
        api_version: make_api_version(1, 1, 0, 0),
    };
    let create = VkInstanceCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        p_application_info: &application,
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: core::ptr::null(),
    };
    let mut output = core::ptr::null_mut();

    assert_eq!(
        crate::instance::vkCreateInstance(
            &create as *const _ as *const c_void,
            core::ptr::null(),
            &mut output,
        ),
        VK_SUCCESS
    );
    assert!(!output.is_null());
    assert_eq!(
        crate::state::StateStore::with(|state| {
            state.instance.as_ref().unwrap().app_api_version
        }),
        make_api_version(0, 1, 0, 0)
    );
    crate::instance::vkDestroyInstance(output, core::ptr::null());
}

#[test]
fn instance_accepts_newer_header_patch_for_advertised_api() {
    let _g = test_guard();
    let application = VkApplicationInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        p_application_name: core::ptr::null(),
        application_version: 0,
        p_engine_name: core::ptr::null(),
        engine_version: 0,
        api_version: HL_API_VERSION | 0x0fff,
    };
    let create = VkInstanceCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        p_application_info: &application,
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: core::ptr::null(),
    };
    let mut output = core::ptr::null_mut();

    assert_eq!(
        crate::instance::vkCreateInstance(
            &create as *const _ as *const c_void,
            core::ptr::null(),
            &mut output,
        ),
        VK_SUCCESS
    );
    assert!(!output.is_null());
    // A patch version names a header revision, not capability, so it is clamped away with the rest: the
    // instance records the version this driver actually honours.
    assert_eq!(
        crate::state::StateStore::with(|state| {
            state.instance.as_ref().unwrap().app_api_version
        }),
        HL_API_VERSION
    );

    crate::instance::vkDestroyInstance(output, core::ptr::null());
}

#[test]
fn device_rejects_unknown_extension_without_creating_state() {
    let _g = test_guard();
    crate::state::StateStore::with(|state| state.clear_devices());
    let unknown = std::ffi::CString::new("VK_HL_not_present").unwrap();
    let names = [unknown.as_ptr()];
    let create = VkDeviceCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        queue_create_info_count: 0,
        p_queue_create_infos: core::ptr::null(),
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 1,
        pp_enabled_extension_names: names.as_ptr(),
        p_enabled_features: core::ptr::null(),
    };
    let mut output = core::ptr::null_mut();

    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            &create as *const _ as *const c_void,
            core::ptr::null(),
            &mut output,
        ),
        VK_ERROR_EXTENSION_NOT_PRESENT
    );
    assert!(output.is_null());
    assert!(crate::state::StateStore::with(|state| !state.has_device()));
}

#[test]
fn device_rejects_unadvertised_base_feature_without_creating_state() {
    let _g = test_guard();
    crate::state::StateStore::with(|state| state.clear_devices());
    let mut features = VkPhysicalDeviceFeatures {
        bits: [VK_FALSE; 55],
    };
    features.bits[4] = VK_TRUE; // geometryShader is not advertised.
    let create = VkDeviceCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        queue_create_info_count: 0,
        p_queue_create_infos: core::ptr::null(),
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: core::ptr::null(),
        p_enabled_features: &features,
    };
    let mut output = core::ptr::null_mut();

    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            &create as *const _ as *const c_void,
            core::ptr::null(),
            &mut output,
        ),
        VK_ERROR_FEATURE_NOT_PRESENT
    );
    assert!(output.is_null());
    assert!(crate::state::StateStore::with(|state| !state.has_device()));
}

#[test]
fn descriptor_array_features_are_advertised_independently() {
    use hl_gpu::protocol::model::capability::binding_array;

    for (capability, index) in [
        (binding_array::UNIFORM_BUFFER, 33usize),
        (binding_array::SAMPLED_TEXTURE | binding_array::SAMPLER, 34),
        (binding_array::STORAGE_BUFFER, 35),
        (binding_array::STORAGE_TEXTURE, 36),
    ] {
        let mut features = VkPhysicalDeviceFeatures {
            bits: [VK_FALSE; 55],
        };
        features.enable_binding_arrays(capability);
        for candidate in 33..=36 {
            assert_eq!(
                features.bits[candidate] != VK_FALSE,
                candidate == index,
                "capability {capability:#x} leaked into feature index {candidate}"
            );
        }
    }
}

#[test]
fn shader_execution_features_are_advertised_independently() {
    use hl_gpu::protocol::model::capability::gpu_feature;

    for (capability, index) in [
        (gpu_feature::ROBUST_BUFFER_ACCESS, 0usize),
        (gpu_feature::FRAGMENT_STORES_ATOMICS, 26usize),
        (gpu_feature::DEPTH_BIAS_CLAMP, 12usize),
        (gpu_feature::IMAGE_CUBE_ARRAY, 2usize),
        (gpu_feature::INDEPENDENT_BLEND, 3usize),
        (gpu_feature::SAMPLE_RATE_SHADING, 6usize),
    ] {
        let mut features = VkPhysicalDeviceFeatures {
            bits: [VK_FALSE; 55],
        };
        features.enable_shader_guarantees(capability);
        for candidate in [0usize, 2, 3, 6, 12, 26] {
            assert_eq!(
                features.bits[candidate] != VK_FALSE,
                candidate == index,
                "capability {capability:#x} leaked into feature index {candidate}"
            );
        }
    }
}

#[test]
fn physical_feature_query_uses_negotiated_shader_guarantees() {
    use hl_gpu::protocol::model::capability::gpu_feature;

    let mut caps = hl_gpu::Capabilities::permissive_fixture("test");
    caps.gpu_features = gpu_feature::ROBUST_BUFFER_ACCESS;
    let robust = VkPhysicalDeviceFeatures::advertised(Some(&caps));
    assert_ne!(robust.bits[0], VK_FALSE);
    assert_eq!(robust.bits[26], VK_FALSE);

    caps.gpu_features = gpu_feature::FRAGMENT_STORES_ATOMICS;
    let fragment = VkPhysicalDeviceFeatures::advertised(Some(&caps));
    assert_eq!(fragment.bits[0], VK_FALSE);
    assert_ne!(fragment.bits[26], VK_FALSE);

    let unavailable = VkPhysicalDeviceFeatures::advertised(None);
    assert_eq!(unavailable.bits[0], VK_FALSE);
    assert_eq!(unavailable.bits[26], VK_FALSE);
}

#[test]
fn device_request_forwards_enabled_shader_guarantees() {
    use hl_gpu::protocol::model::capability::gpu_feature;

    let mut features = VkPhysicalDeviceFeatures {
        bits: [VK_FALSE; 55],
    };
    features.bits[0] = VK_TRUE;
    features.bits[26] = VK_TRUE;
    features.bits[12] = VK_TRUE;
    features.bits[2] = VK_TRUE;
    features.bits[3] = VK_TRUE;
    features.bits[6] = VK_TRUE;
    let create = VkDeviceCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        queue_create_info_count: 0,
        p_queue_create_infos: core::ptr::null(),
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: core::ptr::null(),
        p_enabled_features: &features,
    };
    assert_eq!(
        crate::device::Request::new(&create).gpu_features(),
        gpu_feature::ROBUST_BUFFER_ACCESS
            | gpu_feature::FRAGMENT_STORES_ATOMICS
            | gpu_feature::DEPTH_BIAS_CLAMP
            | gpu_feature::IMAGE_CUBE_ARRAY
            | gpu_feature::INDEPENDENT_BLEND
            | gpu_feature::SAMPLE_RATE_SHADING
    );
}

#[test]
fn device_rejects_unadvertised_features2_feature_without_creating_state() {
    let _g = test_guard();
    crate::state::StateStore::with(|state| state.clear_devices());
    let mut requested = VkPhysicalDeviceFeatures2 {
        s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
        p_next: core::ptr::null_mut(),
        features: VkPhysicalDeviceFeatures {
            bits: [VK_FALSE; 55],
        },
    };
    requested.features.bits[4] = VK_TRUE;
    let create = VkDeviceCreateInfo {
        s_type: 0,
        p_next: &requested as *const _ as *const c_void,
        flags: 0,
        queue_create_info_count: 0,
        p_queue_create_infos: core::ptr::null(),
        enabled_layer_count: 0,
        pp_enabled_layer_names: core::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: core::ptr::null(),
        p_enabled_features: core::ptr::null(),
    };
    let mut output = core::ptr::null_mut();

    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            &create as *const _ as *const c_void,
            core::ptr::null(),
            &mut output,
        ),
        VK_ERROR_FEATURE_NOT_PRESENT
    );
    assert!(output.is_null());
    assert!(crate::state::StateStore::with(|state| !state.has_device()));
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

#[path = "tests_commands.rs"]
mod commands;

/// Build a poisoned `VkMemoryDedicatedRequirements`: a driver that does not write it leaves a value
/// that is not a valid `VkBool32`, which is exactly what the conformance suite caught.
fn poisoned_dedicated() -> VkMemoryDedicatedRequirements {
    VkMemoryDedicatedRequirements {
        s_type: VK_STRUCTURE_TYPE_MEMORY_DEDICATED_REQUIREMENTS,
        p_next: core::ptr::null_mut(),
        prefers_dedicated_allocation: 0xDEAD_BEEF,
        requires_dedicated_allocation: 0xDEAD_BEEF,
    }
}

fn buffer_create_info(size: u64) -> VkBufferCreateInfo {
    VkBufferCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        size,
        usage: 0,
        sharing_mode: 0,
        queue_family_index_count: 0,
        p_queue_family_indices: core::ptr::null(),
    }
}

/// A `VkMemoryDedicatedRequirements` chained onto a `VkMemoryRequirements2` is an OUTPUT the driver owes
/// the caller, not an input it may leave alone. Skipping it hands back whatever the caller's stack held,
/// which is how `dEQP-VK.memory.requirements.dedicated_allocation.buffer.regular` came to fail
/// `validValueVkBool32(...)` — a check that fires only when a `VkBool32` is neither 0 nor 1.
#[test]
fn device_memory_requirements_answers_a_chained_dedicated_requirements() {
    let ci = buffer_create_info(4096);
    let info = VkDeviceBufferMemoryRequirements {
        s_type: 0,
        p_next: core::ptr::null(),
        p_create_info: &ci,
    };
    let mut dedicated = poisoned_dedicated();
    let mut out: VkMemoryRequirements2 = unsafe { core::mem::zeroed() };
    out.p_next = &mut dedicated as *mut _ as *mut c_void;

    crate::maintenance::vkGetDeviceBufferMemoryRequirements(
        core::ptr::null_mut(),
        &info as *const _ as *const c_void,
        &mut out as *mut _ as *mut c_void,
    );

    // The BASE structure first. Without this the test would pass against a driver that answered the
    // chain and produced nothing else, and the query itself would be untested.
    assert_eq!(
        out.memory_requirements.size, 4096,
        "the base requirements must still be filled"
    );
    assert_eq!(out.memory_requirements.alignment, 256);
    // Then the chained output. Both are VK_FALSE for this driver — every resource is a suballocation of
    // ordinary host memory — but they must be WRITTEN, not left as the caller found them.
    assert_eq!(
        dedicated.prefers_dedicated_allocation, 0,
        "prefersDedicatedAllocation was not written"
    );
    assert_eq!(
        dedicated.requires_dedicated_allocation, 0,
        "requiresDedicatedAllocation was not written"
    );
}

/// The per-object entry points answer the chain too, and they reach it through two different modules.
/// A handle this driver cannot resolve is used deliberately: the base size is then 0, but an output
/// structure is still owed — "I do not know this buffer" is not a licence to leave the caller's memory
/// as it was found. The base fields that do not depend on the handle prove the query still ran.
#[test]
fn per_object_memory_requirements2_answer_a_chained_dedicated_requirements() {
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

    let mut dedicated = poisoned_dedicated();
    let mut out: VkMemoryRequirements2 = unsafe { core::mem::zeroed() };
    out.p_next = &mut dedicated as *mut _ as *mut c_void;
    let info = VkBufferMemoryRequirementsInfo2 {
        s_type: 0,
        p_next: core::ptr::null(),
        buffer: 0,
    };
    crate::compute::vkGetBufferMemoryRequirements2(
        dev,
        &info as *const _ as *const c_void,
        &mut out as *mut _ as *mut c_void,
    );
    assert_eq!(
        out.memory_requirements.alignment, 256,
        "the base fill must have run"
    );
    assert_ne!(out.memory_requirements.memory_type_bits, 0);
    assert_eq!(
        dedicated.prefers_dedicated_allocation, 0,
        "vkGetBufferMemoryRequirements2 left the chain unwritten"
    );
    assert_eq!(dedicated.requires_dedicated_allocation, 0);

    let mut dedicated = poisoned_dedicated();
    let mut out: VkMemoryRequirements2 = unsafe { core::mem::zeroed() };
    out.p_next = &mut dedicated as *mut _ as *mut c_void;
    let info = VkImageMemoryRequirementsInfo2 {
        s_type: 0,
        p_next: core::ptr::null(),
        image: 0,
    };
    crate::graphics::vkGetImageMemoryRequirements2(
        dev,
        &info as *const _ as *const c_void,
        &mut out as *mut _ as *mut c_void,
    );
    assert_eq!(
        dedicated.prefers_dedicated_allocation, 0,
        "vkGetImageMemoryRequirements2 left the chain unwritten"
    );
    assert_eq!(dedicated.requires_dedicated_allocation, 0);
}

/// An UNRECOGNISED structure must be left exactly as the caller left it AND must not stop the walk.
/// Vulkan requires an implementation to skip what it does not know; a driver that halted on the first
/// unknown would silently drop every output behind it, and the caller could not tell the difference
/// between "skipped because unknown" and "skipped because the walk ended". The poisoned structure sits
/// AFTER the unknown node, so it is reached only if the walk continues.
#[test]
fn an_unrecognised_chain_node_is_skipped_without_stopping_the_walk() {
    #[repr(C)]
    struct Unknown {
        s_type: i32,
        p_next: *mut c_void,
        sentinel: u64,
    }
    let ci = buffer_create_info(2048);
    let info = VkDeviceBufferMemoryRequirements {
        s_type: 0,
        p_next: core::ptr::null(),
        p_create_info: &ci,
    };
    let mut dedicated = poisoned_dedicated();
    let mut unknown = Unknown {
        // A structure type this driver has never heard of.
        s_type: 0x7FFF_0000,
        p_next: &mut dedicated as *mut _ as *mut c_void,
        sentinel: 0x0BAD_F00D,
    };
    let mut out: VkMemoryRequirements2 = unsafe { core::mem::zeroed() };
    out.p_next = &mut unknown as *mut _ as *mut c_void;

    crate::maintenance::vkGetDeviceBufferMemoryRequirements(
        core::ptr::null_mut(),
        &info as *const _ as *const c_void,
        &mut out as *mut _ as *mut c_void,
    );

    assert_eq!(out.memory_requirements.size, 2048, "the query still ran");
    assert_eq!(
        unknown.sentinel, 0x0BAD_F00D,
        "an unrecognised structure must be left untouched"
    );
    assert_eq!(
        dedicated.prefers_dedicated_allocation, 0,
        "the walk must continue past an unrecognised node"
    );
}
