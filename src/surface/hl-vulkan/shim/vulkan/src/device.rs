//! Logical-device + queue bring-up: `vkCreateDevice` / `vkDestroyDevice` / `vkGetDeviceQueue`.
//!
//! `vkCreateDevice` builds the `hl_vulkan::Device` (the object model + lowering target) over the
//! instance's physical device and stores it in the process-global [`crate::state`]. The device + queue
//! handles are loader-magic'd dispatchable tokens.

use core::ffi::c_void;
use std::ffi::CStr;

use hl_gpu::protocol::model::capability::{binding_array, gpu_feature};
use hl_gpu::{CommandSink, FeatureRequest, PresentKind, WIRE_VERSION};
use hl_vulkan::Instance;

use crate::feature_structs::FeatureStruct;
use crate::promoted_features::PromotedFeatures;
use crate::state::StateStore;
use crate::types::*;

/// The guest's `vkCreateDevice` request, read against what this driver actually performs.
///
/// `vkCreateDevice` must refuse a feature or extension the driver cannot honour, and the same feature
/// chain decides which GPU capabilities the session has to negotiate. Every one of those rules walks the
/// one `pNext` chain under the same aliasing invariant, so they belong to the request rather than to the
/// entry point that receives it.
pub(crate) struct Request<'a> {
    create_info: &'a VkDeviceCreateInfo,
}

impl<'a> Request<'a> {
    pub(crate) fn new(create_info: &'a VkDeviceCreateInfo) -> Self {
        Self { create_info }
    }

    fn validates_features(&self) -> bool {
        let create_info = self.create_info;
        let supported = crate::instance::supported_features();
        if let Some(requested) = unsafe { create_info.p_enabled_features.as_ref() } {
            if requested
                .bits
                .iter()
                .zip(supported.bits)
                .any(|(&requested, supported)| requested != VK_FALSE && supported == VK_FALSE)
            {
                return false;
            }
        }

        let mut node = create_info.p_next as *const VkBaseInStructure;
        while let Some(header) = unsafe { node.as_ref() } {
            if header.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 {
                let features = unsafe { &*(node as *const VkPhysicalDeviceFeatures2) };
                if features
                    .features
                    .bits
                    .iter()
                    .zip(supported.bits)
                    .any(|(&requested, supported)| requested != VK_FALSE && supported == VK_FALSE)
                {
                    return false;
                }
            } else if let Some(feature) = FeatureStruct::matching(header.s_type) {
                if let Some(index) = unsafe { feature.first_unimplemented_request(node) } {
                    crate::stub::Call::unsupported(
                        "vkCreateDevice",
                        &format!("{} feature bit {index}", feature.name),
                    );
                    return false;
                }
            } else if let Some(aggregate) = PromotedFeatures::matching(header.s_type) {
                // A promoted feature enabled through `VkPhysicalDeviceVulkan1{1,2,3,4}Features`. Vulkan
                // requires `VK_ERROR_FEATURE_NOT_PRESENT` here; ignoring the aggregate returned
                // `VK_SUCCESS` for a feature that was then silently absent at draw time.
                if let Some(member) = unsafe { aggregate.first_unimplemented_request(node) } {
                    crate::stub::Call::unsupported("vkCreateDevice", &member);
                    return false;
                }
            }
            node = header.p_next;
        }
        true
    }

    fn validates_extensions(&self) -> bool {
        let create_info = self.create_info;
        if create_info.enabled_extension_count == 0 {
            return true;
        }
        if create_info.pp_enabled_extension_names.is_null() {
            return false;
        }
        let names = unsafe {
            std::slice::from_raw_parts(
                create_info.pp_enabled_extension_names,
                create_info.enabled_extension_count as usize,
            )
        };
        names.iter().all(|&name| {
            if name.is_null() {
                return false;
            }
            let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
                return false;
            };
            hl_vulkan::model::capability::DEVICE_EXTENSIONS
                .iter()
                .any(|extension| extension.name == name)
        })
    }

    fn binding_arrays(&self) -> u32 {
        let create_info = self.create_info;
        let mut bits = 0;
        let mut include = |features: &VkPhysicalDeviceFeatures| {
            if features.bits[33] != VK_FALSE {
                bits |= binding_array::UNIFORM_BUFFER;
            }
            if features.bits[35] != VK_FALSE {
                bits |= binding_array::STORAGE_BUFFER;
            }
            if features.bits[34] != VK_FALSE {
                bits |= binding_array::SAMPLED_TEXTURE | binding_array::SAMPLER;
            }
            if features.bits[36] != VK_FALSE {
                bits |= binding_array::STORAGE_TEXTURE;
            }
        };
        if let Some(features) = unsafe { create_info.p_enabled_features.as_ref() } {
            include(features);
        }
        let mut node = create_info.p_next as *const VkBaseInStructure;
        while let Some(header) = unsafe { node.as_ref() } {
            if header.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 {
                include(&unsafe { &*(node as *const VkPhysicalDeviceFeatures2) }.features);
            }
            node = header.p_next;
        }
        bits
    }

    pub(crate) fn gpu_features(&self) -> u32 {
        let create_info = self.create_info;
        let mut bits = 0;
        let mut include = |features: &VkPhysicalDeviceFeatures| {
            if features.bits[0] != VK_FALSE {
                bits |= gpu_feature::ROBUST_BUFFER_ACCESS;
            }
            if features.bits[26] != VK_FALSE {
                bits |= gpu_feature::FRAGMENT_STORES_ATOMICS;
            }
            if features.bits[12] != VK_FALSE {
                bits |= gpu_feature::DEPTH_BIAS_CLAMP;
            }
            if features.bits[2] != VK_FALSE {
                bits |= gpu_feature::IMAGE_CUBE_ARRAY;
            }
            if features.bits[3] != VK_FALSE {
                bits |= gpu_feature::INDEPENDENT_BLEND;
            }
            if features.bits[6] != VK_FALSE {
                bits |= gpu_feature::SAMPLE_RATE_SHADING;
            }
        };
        if let Some(features) = unsafe { create_info.p_enabled_features.as_ref() } {
            include(features);
        }
        let mut node = create_info.p_next as *const VkBaseInStructure;
        while let Some(header) = unsafe { node.as_ref() } {
            if header.s_type == VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 {
                include(&unsafe { &*(node as *const VkPhysicalDeviceFeatures2) }.features);
            }
            node = header.p_next;
        }
        bits
    }
}

pub extern "C" fn vkCreateDevice(
    _physical_device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_device: *mut *mut c_void,
) -> VkResult {
    if p_device.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let create_info = unsafe { (p_create_info as *const VkDeviceCreateInfo).as_ref() };
    let (binding_arrays, gpu_features) = match create_info {
        Some(create_info) => {
            let request = Request::new(create_info);
            if !request.validates_extensions() {
                return VK_ERROR_EXTENSION_NOT_PRESENT;
            }
            if !request.validates_features() {
                return VK_ERROR_FEATURE_NOT_PRESENT;
            }
            (request.binding_arrays(), request.gpu_features())
        }
        // Vulkan permits a null `pCreateInfo`; nothing is requested, so nothing is negotiated.
        None => (0, 0),
    };
    let token = StateStore::with(|s| {
        // Build the logical device over the instance's physical device (materialize a default instance
        // if a device is somehow requested before `vkCreateInstance`).
        let inst = s
            .instance
            .get_or_insert_with(|| Instance::new(HL_API_VERSION))
            .clone();
        // Negotiate ONCE per process. The sink is process-global, so a second `vkCreateDevice`
        // re-negotiating over a connection already in use is what put the driver into
        // VK_ERROR_DEVICE_LOST for every object created on a second device.
        let native_present = if !s.negotiated
            && (std::env::var_os("HL_GPU_EXEC").is_some()
                || binding_arrays != 0
                || gpu_features != 0)
        {
            let Ok(capabilities) = s.sink.negotiate(&FeatureRequest {
                wire_version: WIRE_VERSION,
                binding_arrays,
                gpu_features,
                ..FeatureRequest::default()
            }) else {
                return None;
            };
            s.negotiated = true;
            capabilities.present_kinds.contains(&PresentKind::IoSurface)
        } else {
            s.native_present
        };
        s.native_present = native_present;
        // A device of its own, under a token of its own. Any device already live keeps its object model.
        Some(s.insert_device(inst.create_device_with_ir_ids(s.ir_ids.clone())))
    });
    let Some(token) = token else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    unsafe { *p_device = token };
    VK_SUCCESS
}

/// `vkDestroyDevice` destroys the device it is GIVEN, and only that one. Destroying whichever device
/// happened to be current left an application's other handle dangling; the loader then rejected it and
/// aborted the process, which is what ended every `object_management.single.*` CTS case.
pub extern "C" fn vkDestroyDevice(device: *mut c_void, _p_allocator: *const c_void) {
    StateStore::with(|s| s.remove_device(device));
}

pub extern "C" fn vkGetDeviceQueue(
    _device: *mut c_void,
    _queue_family_index: u32,
    _queue_index: u32,
    p_queue: *mut *mut c_void,
) {
    if p_queue.is_null() {
        return;
    }
    let q = StateStore::with(|s| s.queue_token());
    unsafe { *p_queue = q };
}

/// `vkGetDeviceQueue2` (Vulkan 1.1) — the `VkDeviceQueueInfo2`-parameterized retrieval. The device
/// exposes exactly one queue (family 0, index 0), so this returns the same lone queue token as
/// `vkGetDeviceQueue`; a request for any other `(family, index)` returns `VK_NULL_HANDLE`.
pub extern "C" fn vkGetDeviceQueue2(
    _device: *mut c_void,
    p_queue_info: *const c_void,
    p_queue: *mut *mut c_void,
) {
    if p_queue.is_null() {
        return;
    }
    unsafe { *p_queue = core::ptr::null_mut() };
    let Some(info) = (unsafe { (p_queue_info as *const VkDeviceQueueInfo2).as_ref() }) else {
        return;
    };
    if info.queue_family_index != 0 || info.queue_index != 0 {
        return; // only the single (family 0, index 0) queue exists.
    }
    let q = StateStore::with(|s| s.queue_token());
    unsafe { *p_queue = q };
}
