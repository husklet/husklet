//! The Vulkan 1.1–1.4 promoted-feature aggregate structs (`VkPhysicalDeviceVulkan1{1,2,3,4}Features`).
//!
//! Each is `{ sType, pNext, VkBool32[N] }` with the members in exact `vk.xml` declaration order. An
//! application enables a promoted feature through one of these rather than through
//! `VkPhysicalDeviceFeatures2`, so both directions must honour them:
//!
//!   * `vkCreateDevice` must return `VK_ERROR_FEATURE_NOT_PRESENT` for a requested member this driver
//!     does not implement. Ignoring the chain returns `VK_SUCCESS` for a feature that is then silently
//!     absent — the application renders wrong output with no error.
//!   * `vkGetPhysicalDeviceFeatures2` must report the same answer through the aggregate spelling as
//!     through the single-feature spelling, or a client sees a self-contradicting device.
//!
//! Implemented members match capabilities actually served by the driver: Vulkan 1.2
//! `samplerMirrorClampToEdge` and Vulkan 1.3 `dynamicRendering`. Every other member is absent.

use core::ffi::c_void;

use crate::types::{VkBaseInStructure, VkBool32, VK_FALSE};

/// `VkStructureType` values of the four aggregates (stable core values 49/51/53/55 from `vk.xml`).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES: i32 = 49;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES: i32 = 51;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES: i32 = 53;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_4_FEATURES: i32 = 55;

/// `VkPhysicalDeviceVulkan13Features::dynamicRendering` — index 12 in `vk.xml` declaration order.
pub const VULKAN_1_3_DYNAMIC_RENDERING: usize = 12;
/// `VkPhysicalDeviceVulkan12Features::samplerMirrorClampToEdge` — first member in `vk.xml` order.
pub const VULKAN_1_2_SAMPLER_MIRROR_CLAMP_TO_EDGE: usize = 0;

/// One promoted-feature aggregate laid out as the C ABI declares it. `N` is the member count, so the
/// trailing bool array is typed rather than reached by hand-computed offsets.
#[repr(C)]
struct Aggregate<const N: usize> {
    s_type: i32,
    p_next: *mut c_void,
    bits: [VkBool32; N],
}

/// The `vk.xml` member names of `VkPhysicalDeviceVulkan11Features`, index-aligned with its bool array.
const VULKAN_1_1: &[&str] = &[
    "storageBuffer16BitAccess",
    "uniformAndStorageBuffer16BitAccess",
    "storagePushConstant16",
    "storageInputOutput16",
    "multiview",
    "multiviewGeometryShader",
    "multiviewTessellationShader",
    "variablePointersStorageBuffer",
    "variablePointers",
    "protectedMemory",
    "samplerYcbcrConversion",
    "shaderDrawParameters",
];

/// The `vk.xml` member names of `VkPhysicalDeviceVulkan12Features`.
const VULKAN_1_2: &[&str] = &[
    "samplerMirrorClampToEdge",
    "drawIndirectCount",
    "storageBuffer8BitAccess",
    "uniformAndStorageBuffer8BitAccess",
    "storagePushConstant8",
    "shaderBufferInt64Atomics",
    "shaderSharedInt64Atomics",
    "shaderFloat16",
    "shaderInt8",
    "descriptorIndexing",
    "shaderInputAttachmentArrayDynamicIndexing",
    "shaderUniformTexelBufferArrayDynamicIndexing",
    "shaderStorageTexelBufferArrayDynamicIndexing",
    "shaderUniformBufferArrayNonUniformIndexing",
    "shaderSampledImageArrayNonUniformIndexing",
    "shaderStorageBufferArrayNonUniformIndexing",
    "shaderStorageImageArrayNonUniformIndexing",
    "shaderInputAttachmentArrayNonUniformIndexing",
    "shaderUniformTexelBufferArrayNonUniformIndexing",
    "shaderStorageTexelBufferArrayNonUniformIndexing",
    "descriptorBindingUniformBufferUpdateAfterBind",
    "descriptorBindingSampledImageUpdateAfterBind",
    "descriptorBindingStorageImageUpdateAfterBind",
    "descriptorBindingStorageBufferUpdateAfterBind",
    "descriptorBindingUniformTexelBufferUpdateAfterBind",
    "descriptorBindingStorageTexelBufferUpdateAfterBind",
    "descriptorBindingUpdateUnusedWhilePending",
    "descriptorBindingPartiallyBound",
    "descriptorBindingVariableDescriptorCount",
    "runtimeDescriptorArray",
    "samplerFilterMinmax",
    "scalarBlockLayout",
    "imagelessFramebuffer",
    "uniformBufferStandardLayout",
    "shaderSubgroupExtendedTypes",
    "separateDepthStencilLayouts",
    "hostQueryReset",
    "timelineSemaphore",
    "bufferDeviceAddress",
    "bufferDeviceAddressCaptureReplay",
    "bufferDeviceAddressMultiDevice",
    "vulkanMemoryModel",
    "vulkanMemoryModelDeviceScope",
    "vulkanMemoryModelAvailabilityVisibilityChains",
    "shaderOutputViewportIndex",
    "shaderOutputLayer",
    "subgroupBroadcastDynamicId",
];

/// The `vk.xml` member names of `VkPhysicalDeviceVulkan13Features`.
const VULKAN_1_3: &[&str] = &[
    "robustImageAccess",
    "inlineUniformBlock",
    "descriptorBindingInlineUniformBlockUpdateAfterBind",
    "pipelineCreationCacheControl",
    "privateData",
    "shaderDemoteToHelperInvocation",
    "shaderTerminateInvocation",
    "subgroupSizeControl",
    "computeFullSubgroups",
    "synchronization2",
    "textureCompressionASTC_HDR",
    "shaderZeroInitializeWorkgroupMemory",
    "dynamicRendering",
    "shaderIntegerDotProduct",
    "maintenance4",
];

/// The `vk.xml` member names of `VkPhysicalDeviceVulkan14Features`.
const VULKAN_1_4: &[&str] = &[
    "globalPriorityQuery",
    "shaderSubgroupRotate",
    "shaderSubgroupRotateClustered",
    "shaderFloatControls2",
    "shaderExpectAssume",
    "rectangularLines",
    "bresenhamLines",
    "smoothLines",
    "stippledRectangularLines",
    "stippledBresenhamLines",
    "stippledSmoothLines",
    "vertexAttributeInstanceRateDivisor",
    "vertexAttributeInstanceRateZeroDivisor",
    "indexTypeUint8",
    "dynamicRenderingLocalRead",
    "maintenance5",
    "maintenance6",
    "pipelineProtectedAccess",
    "pipelineRobustness",
    "hostImageCopy",
    "pushDescriptor",
];

/// One promoted-feature aggregate: its `sType`, its members in `vk.xml` order, and the member indices
/// this driver really implements.
pub struct PromotedFeatures {
    s_type: i32,
    struct_name: &'static str,
    members: &'static [&'static str],
    implemented: &'static [usize],
}

impl PromotedFeatures {
    /// Every aggregate this driver recognizes. A `sType` absent from this table is left untouched: it
    /// is neither validated nor reported, which is why an unrecognized features struct must not be
    /// treated as satisfied.
    pub const ALL: &'static [Self] = &[
        Self {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES,
            struct_name: "VkPhysicalDeviceVulkan11Features",
            members: VULKAN_1_1,
            implemented: &[],
        },
        Self {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
            struct_name: "VkPhysicalDeviceVulkan12Features",
            members: VULKAN_1_2,
            implemented: &[VULKAN_1_2_SAMPLER_MIRROR_CLAMP_TO_EDGE],
        },
        Self {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            struct_name: "VkPhysicalDeviceVulkan13Features",
            members: VULKAN_1_3,
            implemented: &[VULKAN_1_3_DYNAMIC_RENDERING],
        },
        Self {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_4_FEATURES,
            struct_name: "VkPhysicalDeviceVulkan14Features",
            members: VULKAN_1_4,
            implemented: &[],
        },
    ];

    /// The aggregate a pNext node's `sType` names, if it is one of the four.
    pub fn matching(s_type: i32) -> Option<&'static Self> {
        Self::ALL.iter().find(|entry| entry.s_type == s_type)
    }

    pub fn struct_name(&self) -> &'static str {
        self.struct_name
    }

    /// The `struct::member` name of the first member set `VK_TRUE` that this driver does not implement.
    ///
    /// # Safety
    /// `node` must address a live struct of the type this aggregate's `sType` names.
    pub unsafe fn first_unimplemented_request(
        &self,
        node: *const VkBaseInStructure,
    ) -> Option<String> {
        let requested = self.requested(node);
        requested.iter().enumerate().find_map(|(index, &bit)| {
            (bit != VK_FALSE && !self.implemented.contains(&index)).then(|| {
                let member = self.members.get(index).copied().unwrap_or("?");
                format!("{}::{member}", self.struct_name)
            })
        })
    }

    /// Report this driver's answer into an application-supplied aggregate: every implemented member
    /// `VK_TRUE`, every other member `VK_FALSE`. Overwrites the whole bool array so the report never
    /// depends on how the application initialized it.
    ///
    /// # Safety
    /// Same contract as [`Self::first_unimplemented_request`], and `node` must be writable.
    pub unsafe fn report(&self, node: *mut c_void) {
        let count = self.members.len();
        let bits = node.cast::<u8>().add(Self::BITS_OFFSET).cast::<VkBool32>();
        for index in 0..count {
            bits.add(index)
                .write(VkBool32::from(self.implemented.contains(&index)));
        }
    }

    /// Byte offset of the trailing bool array in every aggregate — taken from the typed `#[repr(C)]`
    /// layout rather than assumed, so it stays correct on any target ABI.
    const BITS_OFFSET: usize = core::mem::size_of::<Aggregate<0>>();

    /// The application's requested bits, read through the typed layout for this aggregate's length.
    ///
    /// # Safety
    /// Same contract as [`Self::first_unimplemented_request`].
    unsafe fn requested(&self, node: *const VkBaseInStructure) -> Vec<VkBool32> {
        let count = self.members.len();
        let bits = node.cast::<u8>().add(Self::BITS_OFFSET).cast::<VkBool32>();
        (0..count).map(|index| bits.add(index).read()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::VK_TRUE;

    /// The typed layout must place the bool array where the C ABI does: `sType` (4) + padding (4) +
    /// `pNext` (8) on a 64-bit target. A wrong offset would read the application's `pNext` as a bool.
    #[test]
    fn bits_follow_stype_and_pnext() {
        assert_eq!(PromotedFeatures::BITS_OFFSET, 16);
        let probe = Aggregate::<4> {
            s_type: 0,
            p_next: core::ptr::null_mut(),
            bits: [VK_FALSE; 4],
        };
        let base = &probe as *const Aggregate<4> as usize;
        let bits = probe.bits.as_ptr() as usize;
        assert_eq!(bits - base, PromotedFeatures::BITS_OFFSET);
    }

    #[test]
    fn member_counts_match_the_registry() {
        // vk.xml VK_HEADER_VERSION 341 member counts.
        for (s_type, count) in [
            (VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES, 12),
            (VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, 47),
            (VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES, 15),
            (VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_4_FEATURES, 21),
        ] {
            let entry = PromotedFeatures::matching(s_type).expect("aggregate is recognized");
            assert_eq!(entry.members.len(), count, "{}", entry.struct_name);
        }
    }

    /// An application that enables a promoted feature through the aggregate spelling must be told the
    /// truth. Before this was honoured, `vkCreateDevice` ignored the aggregate and returned
    /// `VK_SUCCESS`, so the application ran believing `synchronization2` was active.
    #[test]
    fn device_creation_rejects_an_unimplemented_aggregate_feature() {
        let _guard = crate::tests::test_guard();
        crate::state::StateStore::with(|state| state.clear_devices());
        let mut requested = Aggregate::<15> {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            p_next: core::ptr::null_mut(),
            bits: [VK_FALSE; 15],
        };
        requested.bits[9] = VK_TRUE; // synchronization2
        let mut device = core::ptr::null_mut();

        let result = crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            &create_info(&requested as *const _ as *const c_void) as *const _ as *const c_void,
            core::ptr::null(),
            &mut device,
        );

        assert_eq!(result, crate::types::VK_ERROR_FEATURE_NOT_PRESENT);
        assert!(device.is_null());
        assert!(crate::state::StateStore::with(|state| !state.has_device()));
    }

    /// `dynamicRendering` is really implemented, so requesting it through the aggregate must succeed —
    /// the fix must reject unimplemented members, not the aggregate itself.
    #[test]
    fn device_creation_accepts_dynamic_rendering_through_the_aggregate() {
        let _guard = crate::tests::test_guard();
        crate::state::StateStore::with(|state| state.clear_devices());
        let mut requested = Aggregate::<15> {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            p_next: core::ptr::null_mut(),
            bits: [VK_FALSE; 15],
        };
        requested.bits[VULKAN_1_3_DYNAMIC_RENDERING] = VK_TRUE;
        let mut device = core::ptr::null_mut();

        let result = crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            &create_info(&requested as *const _ as *const c_void) as *const _ as *const c_void,
            core::ptr::null(),
            &mut device,
        );

        assert_eq!(result, crate::types::VK_SUCCESS);
        assert!(!device.is_null());
    }

    /// The same feature must read identically through both spellings: `vkGetPhysicalDeviceFeatures2`
    /// reported `dynamicRendering` only into `VkPhysicalDeviceDynamicRenderingFeatures`, leaving the
    /// aggregate a client actually uses at Vulkan 1.3+ reading `VK_FALSE`.
    #[test]
    fn features2_reports_dynamic_rendering_through_the_aggregate() {
        let _guard = crate::tests::test_guard();
        let mut aggregate = Aggregate::<15> {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
            p_next: core::ptr::null_mut(),
            bits: [VK_TRUE; 15],
        };
        let mut query = crate::types::VkPhysicalDeviceFeatures2 {
            s_type: crate::types::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
            p_next: &mut aggregate as *mut _ as *mut c_void,
            features: crate::types::VkPhysicalDeviceFeatures {
                bits: [VK_FALSE; 55],
            },
        };

        crate::instance::vkGetPhysicalDeviceFeatures2(
            core::ptr::null_mut(),
            &mut query as *mut _ as *mut c_void,
        );

        assert_eq!(aggregate.bits[VULKAN_1_3_DYNAMIC_RENDERING], VK_TRUE);
        // An all-VK_TRUE input must be overwritten with the truth, not left as the client wrote it.
        assert_eq!(aggregate.bits[9], VK_FALSE, "synchronization2");
        assert_eq!(aggregate.bits[14], VK_FALSE, "maintenance4");
    }

    /// CTS treats this promoted extension as supported only when the Vulkan 1.2 aggregate bit is true;
    /// enumerating the KHR name alone is deliberately insufficient in Vulkan 1.2+.
    #[test]
    fn features2_reports_sampler_mirror_clamp_through_the_vulkan12_aggregate() {
        let _guard = crate::tests::test_guard();
        let mut aggregate = Aggregate::<47> {
            s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
            p_next: core::ptr::null_mut(),
            bits: [VK_TRUE; 47],
        };
        let mut query = crate::types::VkPhysicalDeviceFeatures2 {
            s_type: crate::types::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
            p_next: &mut aggregate as *mut _ as *mut c_void,
            features: crate::types::VkPhysicalDeviceFeatures { bits: [VK_FALSE; 55] },
        };

        crate::instance::vkGetPhysicalDeviceFeatures2(
            core::ptr::null_mut(),
            &mut query as *mut _ as *mut c_void,
        );

        assert_eq!(aggregate.bits[VULKAN_1_2_SAMPLER_MIRROR_CLAMP_TO_EDGE], VK_TRUE);
        assert_eq!(aggregate.bits[1], VK_FALSE, "drawIndirectCount");
        assert_eq!(aggregate.bits[46], VK_FALSE, "subgroupBroadcastDynamicId");
    }

    fn create_info(p_next: *const c_void) -> crate::types::VkDeviceCreateInfo {
        crate::types::VkDeviceCreateInfo {
            s_type: 0,
            p_next,
            flags: 0,
            queue_create_info_count: 0,
            p_queue_create_infos: core::ptr::null(),
            enabled_layer_count: 0,
            pp_enabled_layer_names: core::ptr::null(),
            enabled_extension_count: 0,
            pp_enabled_extension_names: core::ptr::null(),
            p_enabled_features: core::ptr::null(),
        }
    }

    #[test]
    fn implemented_members_match_the_two_served_promoted_features() {
        let total: usize = PromotedFeatures::ALL
            .iter()
            .map(|entry| entry.implemented.len())
            .sum();
        assert_eq!(total, 2);
        let v12 = PromotedFeatures::matching(VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES)
            .expect("1.2 aggregate is recognized");
        assert_eq!(
            v12.members[VULKAN_1_2_SAMPLER_MIRROR_CLAMP_TO_EDGE],
            "samplerMirrorClampToEdge"
        );
        let v13 = PromotedFeatures::matching(VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES)
            .expect("1.3 aggregate is recognized");
        assert_eq!(
            v13.members[VULKAN_1_3_DYNAMIC_RENDERING], "dynamicRendering",
            "the implemented index must name dynamicRendering in vk.xml order"
        );
    }
}
