use super::*;
use hl_gpu::protocol::model::capability::binding_array;
use hl_gpu::protocol::model::capability::gpu_feature;
use hl_gpu::{CommandSink, FeatureRequest, WIRE_VERSION};

pub extern "C" fn vkEnumeratePhysicalDevices(
    _instance: *mut c_void,
    p_physical_device_count: *mut u32,
    p_physical_devices: *mut *mut c_void,
) -> VkResult {
    if p_physical_device_count.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    if p_physical_devices.is_null() {
        unsafe { *p_physical_device_count = 1 };
        return VK_SUCCESS;
    }
    let cap = unsafe { *p_physical_device_count };
    if cap < 1 {
        unsafe { *p_physical_device_count = 0 };
        return VK_INCOMPLETE;
    }
    let pd = StateStore::with(|s| s.phys_dev_handle());
    unsafe {
        *p_physical_devices = pd;
        *p_physical_device_count = 1;
    }
    VK_SUCCESS
}

pub extern "C" fn vkGetPhysicalDeviceProperties(
    _physical_device: *mut c_void,
    p_properties: *mut c_void,
) -> VkResult {
    let Some(p) = (unsafe { (p_properties as *mut VkPhysicalDeviceProperties).as_mut() }) else {
        return VK_SUCCESS;
    };
    let desc = StateStore::with(|s| s.physical_device());
    p.api_version = desc.api_version;
    p.driver_version = desc.driver_version;
    p.vendor_id = desc.vendor_id;
    p.device_id = desc.device_id;
    p.device_type = desc.device_type as i32;
    Name::write(&mut p.device_name, &desc.name);
    p.pipeline_cache_uuid = desc.pipeline_cache_uuid;
    p.limits = metal_limits();
    p.sparse_properties = VkPhysicalDeviceSparseProperties::default();
    VK_SUCCESS
}

pub extern "C" fn vkGetPhysicalDeviceFeatures(
    _physical_device: *mut c_void,
    p_features: *mut c_void,
) {
    let Some(f) = (unsafe { (p_features as *mut VkPhysicalDeviceFeatures).as_mut() }) else {
        return;
    };
    *f = supported_features();
    let missing = DawnBaseline::new(f, &metal_limits(), 2 << 30).missing();
    if !missing.is_empty() {
        hl_log::hl_warn!(
            hl_log::tag::GPU,
            "vulkan compatibility=dawn_chrome_150 missing={}",
            missing.join(",")
        );
    }
}

pub(crate) fn supported_features() -> VkPhysicalDeviceFeatures {
    let caps = StateStore::with(|state| {
        state
            .sink
            .negotiate(&FeatureRequest {
                wire_version: WIRE_VERSION,
                ..FeatureRequest::default()
            })
            .ok()
    });
    features_for(caps.as_ref())
}

pub(crate) fn features_for(caps: Option<&hl_gpu::Capabilities>) -> VkPhysicalDeviceFeatures {
    let mut features = VkPhysicalDeviceFeatures {
        bits: [VK_FALSE; 55],
    };
    // The conservative feature subset guaranteed across every executor path (indices into the vk.xml
    // `VkPhysicalDeviceFeatures` order): fullDrawIndexUint32(1), samplerAnisotropy(19), shaderInt16(41).
    // Features whose implementation depends on the selected executor are enabled below from negotiation.
    for i in [1usize, 19, 41] {
        features.bits[i] = VK_TRUE;
    }
    let arrays = caps.map_or(0, |caps| caps.binding_arrays);
    enable_binding_array_features(&mut features, arrays);
    let gpu_features = caps.map_or(0, |caps| caps.gpu_features);
    enable_gpu_features(&mut features, gpu_features);
    let required_bc = hl_gpu::protocol::model::enums::TextureFormat::bits(
        hl_gpu::protocol::model::capability::BC_FORMATS,
    );
    if caps.is_some_and(|caps| caps.texture_formats & required_bc == required_bc) {
        features.bits[22] = VK_TRUE;
    }
    features
}

pub(crate) fn enable_gpu_features(features: &mut VkPhysicalDeviceFeatures, gpu_features: u32) {
    if gpu_features & gpu_feature::ROBUST_BUFFER_ACCESS != 0 {
        features.bits[0] = VK_TRUE;
    }
    if gpu_features & gpu_feature::FRAGMENT_STORES_ATOMICS != 0 {
        features.bits[26] = VK_TRUE;
    }
    if gpu_features & gpu_feature::DEPTH_BIAS_CLAMP != 0 {
        features.bits[12] = VK_TRUE;
    }
    if gpu_features & gpu_feature::IMAGE_CUBE_ARRAY != 0 {
        features.bits[2] = VK_TRUE;
    }
    if gpu_features & gpu_feature::INDEPENDENT_BLEND != 0 {
        features.bits[3] = VK_TRUE;
    }
    if gpu_features & gpu_feature::SAMPLE_RATE_SHADING != 0 {
        features.bits[6] = VK_TRUE;
    }
}

pub(crate) fn enable_binding_array_features(features: &mut VkPhysicalDeviceFeatures, arrays: u32) {
    if arrays & binding_array::UNIFORM_BUFFER != 0 {
        features.bits[33] = VK_TRUE; // shaderUniformBufferArrayDynamicIndexing
    }
    if arrays & binding_array::STORAGE_BUFFER != 0 {
        features.bits[35] = VK_TRUE; // shaderStorageBufferArrayDynamicIndexing
    }
    if arrays & (binding_array::SAMPLED_TEXTURE | binding_array::SAMPLER)
        == binding_array::SAMPLED_TEXTURE | binding_array::SAMPLER
    {
        features.bits[34] = VK_TRUE; // shaderSampledImageArrayDynamicIndexing
    }
    if arrays & binding_array::STORAGE_TEXTURE != 0 {
        features.bits[36] = VK_TRUE; // shaderStorageImageArrayDynamicIndexing
    }
}

pub extern "C" fn vkGetPhysicalDeviceMemoryProperties(
    _physical_device: *mut c_void,
    p_memory_properties: *mut c_void,
) {
    let Some(m) =
        (unsafe { (p_memory_properties as *mut VkPhysicalDeviceMemoryProperties).as_mut() })
    else {
        return;
    };
    let pd = StateStore::with(|s| s.physical_device());
    m.memory_types = [VkMemoryType::default(); VK_MAX_MEMORY_TYPES];
    m.memory_heaps = [VkMemoryHeap::default(); VK_MAX_MEMORY_HEAPS];
    // The STANDARD software-Vulkan memory layout (mirrors lavapipe): one unified DEVICE_LOCAL heap and
    // the standard type set (DEVICE_LOCAL; DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT; HOST_VISIBLE|
    // HOST_COHERENT; HOST_VISIBLE|HOST_COHERENT|HOST_CACHED). Every HOST_VISIBLE type is genuinely
    // mappable via vkMapMemory (all our memory is host RAM). Sourced from the model so a test can assert
    // the advertised set, and so `vkGetPhysicalDeviceMemoryProperties2` stays byte-identical.
    let heaps = pd.memory_heaps();
    let types = pd.memory_types();
    m.memory_heap_count = (heaps.len() as u32).min(VK_MAX_MEMORY_HEAPS as u32);
    for (i, h) in heaps.iter().take(VK_MAX_MEMORY_HEAPS).enumerate() {
        m.memory_heaps[i] = VkMemoryHeap {
            size: h.size,
            flags: h.flags,
        };
    }
    m.memory_type_count = (types.len() as u32).min(VK_MAX_MEMORY_TYPES as u32);
    for (i, t) in types.iter().take(VK_MAX_MEMORY_TYPES).enumerate() {
        m.memory_types[i] = VkMemoryType {
            property_flags: t.property_flags,
            heap_index: t.heap_index,
        };
    }
}

pub extern "C" fn vkGetPhysicalDeviceQueueFamilyProperties(
    _physical_device: *mut c_void,
    p_queue_family_property_count: *mut u32,
    p_queue_family_properties: *mut c_void,
) {
    if p_queue_family_property_count.is_null() {
        return;
    }
    if p_queue_family_properties.is_null() {
        unsafe { *p_queue_family_property_count = 1 };
        return;
    }
    if unsafe { *p_queue_family_property_count } < 1 {
        unsafe { *p_queue_family_property_count = 0 };
        return;
    }
    let qf = StateStore::with(|s| s.physical_device().queue_family);
    let out = p_queue_family_properties as *mut VkQueueFamilyProperties;
    unsafe {
        *out = VkQueueFamilyProperties {
            queue_flags: qf.queue_flags,
            queue_count: qf.queue_count,
            timestamp_valid_bits: qf.timestamp_valid_bits,
            min_image_transfer_granularity: VkExtent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
        };
        *p_queue_family_property_count = 1;
    }
}

/// Device extensions the ICD really backs: `VK_KHR_swapchain` (the present path). Nothing unbacked is
/// advertised. Sourced from [`capability::DEVICE_EXTENSIONS`].
pub extern "C" fn vkEnumerateDeviceExtensionProperties(
    _physical_device: *mut c_void,
    _p_layer_name: *const c_char,
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    let exts: Vec<VkExtensionProperties> = capability::DEVICE_EXTENSIONS
        .iter()
        .map(VkExtensionProperties::from)
        .collect();
    unsafe {
        write_enumeration(
            &exts,
            p_property_count,
            p_properties as *mut VkExtensionProperties,
        )
    }
}

/// Deprecated device-layer enumeration — always empty (spec: return instance layers or none).
pub extern "C" fn vkEnumerateDeviceLayerProperties(
    _physical_device: *mut c_void,
    p_property_count: *mut u32,
    p_properties: *mut c_void,
) -> VkResult {
    unsafe {
        write_enumeration::<VkLayerProperties>(
            &[],
            p_property_count,
            p_properties as *mut VkLayerProperties,
        )
    }
}

// ==================================================================================================
// format + image-format queries
// ==================================================================================================
