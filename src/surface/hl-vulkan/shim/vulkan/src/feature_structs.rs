//! Standalone feature structs accepted by `vkGetPhysicalDeviceFeatures2` and `vkCreateDevice`.
//!
//! Querying a supported struct must overwrite every `VkBool32`, including false answers: leaving
//! caller-poisoned bytes untouched reports arbitrary features. Device creation must reject the same
//! false feature when it is requested through the standalone spelling.

use core::ffi::c_void;

use crate::types::{VK_FALSE, VkBaseInStructure, VkBool32};

#[repr(C)]
struct HeaderWithBits<const N: usize> {
    s_type: i32,
    p_next: *mut c_void,
    bits: [VkBool32; N],
}

/// Registry-derived layout of one standalone feature struct. The count is the number of trailing
/// `VkBool32` members after `{ sType, pNext }` in Vulkan registry header revision 341.
pub(crate) struct FeatureStruct {
    pub(crate) s_type: i32,
    pub(crate) name: &'static str,
    count: usize,
    implemented: &'static [usize],
}

impl FeatureStruct {
    pub(crate) const ALL: &'static [Self] = &[
        feature(1_000_083_000, "VkPhysicalDevice16BitStorageFeatures", 4),
        feature(1_000_177_000, "VkPhysicalDevice8BitStorageFeatures", 3),
        feature(
            1_000_257_000,
            "VkPhysicalDeviceBufferDeviceAddressFeatures",
            3,
        ),
        feature(
            1_000_161_001,
            "VkPhysicalDeviceDescriptorIndexingFeatures",
            20,
        ),
        feature(1_000_261_000, "VkPhysicalDeviceHostQueryResetFeatures", 1),
        feature(1_000_335_000, "VkPhysicalDeviceImageRobustnessFeatures", 1),
        feature(
            1_000_108_000,
            "VkPhysicalDeviceImagelessFramebufferFeatures",
            1,
        ),
        feature(
            1_000_138_000,
            "VkPhysicalDeviceInlineUniformBlockFeatures",
            2,
        ),
        feature(1_000_413_000, "VkPhysicalDeviceMaintenance4Features", 1),
        feature(1_000_053_001, "VkPhysicalDeviceMultiviewFeatures", 3),
        feature(
            1_000_297_000,
            "VkPhysicalDevicePipelineCreationCacheControlFeatures",
            1,
        ),
        feature(1_000_295_000, "VkPhysicalDevicePrivateDataFeatures", 1),
        Self {
            s_type: 1_000_344_000,
            name: "VkPhysicalDeviceRGBA10X6FormatsFeaturesEXT",
            count: 1,
            implemented: &[0],
        },
        feature(1_000_145_001, "VkPhysicalDeviceProtectedMemoryFeatures", 1),
        feature(
            1_000_156_004,
            "VkPhysicalDeviceSamplerYcbcrConversionFeatures",
            1,
        ),
        feature(
            1_000_221_000,
            "VkPhysicalDeviceScalarBlockLayoutFeatures",
            1,
        ),
        feature(
            1_000_241_000,
            "VkPhysicalDeviceSeparateDepthStencilLayoutsFeatures",
            1,
        ),
        feature(
            1_000_180_000,
            "VkPhysicalDeviceShaderAtomicInt64Features",
            2,
        ),
        feature(
            1_000_276_000,
            "VkPhysicalDeviceShaderDemoteToHelperInvocationFeatures",
            1,
        ),
        feature(
            1_000_063_000,
            "VkPhysicalDeviceShaderDrawParametersFeatures",
            1,
        ),
        feature(
            1_000_082_000,
            "VkPhysicalDeviceShaderFloat16Int8Features",
            2,
        ),
        feature(
            1_000_280_000,
            "VkPhysicalDeviceShaderIntegerDotProductFeatures",
            1,
        ),
        feature(
            1_000_175_000,
            "VkPhysicalDeviceShaderSubgroupExtendedTypesFeatures",
            1,
        ),
        feature(
            1_000_215_000,
            "VkPhysicalDeviceShaderTerminateInvocationFeatures",
            1,
        ),
        feature(
            1_000_225_002,
            "VkPhysicalDeviceSubgroupSizeControlFeatures",
            2,
        ),
        feature(1_000_314_007, "VkPhysicalDeviceSynchronization2Features", 1),
        feature(
            1_000_066_000,
            "VkPhysicalDeviceTextureCompressionASTCHDRFeatures",
            1,
        ),
        feature(
            1_000_207_000,
            "VkPhysicalDeviceTimelineSemaphoreFeatures",
            1,
        ),
        feature(
            1_000_253_000,
            "VkPhysicalDeviceUniformBufferStandardLayoutFeatures",
            1,
        ),
        feature(1_000_120_000, "VkPhysicalDeviceVariablePointersFeatures", 2),
        feature(
            1_000_211_000,
            "VkPhysicalDeviceVulkanMemoryModelFeatures",
            3,
        ),
        feature(
            1_000_325_000,
            "VkPhysicalDeviceZeroInitializeWorkgroupMemoryFeatures",
            1,
        ),
        Self {
            s_type: crate::types::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES,
            name: "VkPhysicalDeviceDynamicRenderingFeatures",
            count: 1,
            implemented: &[0],
        },
    ];

    pub(crate) fn matching(s_type: i32) -> Option<&'static Self> {
        Self::ALL.iter().find(|feature| feature.s_type == s_type)
    }

    /// # Safety
    /// `node` must point to a writable struct identified by this entry's `sType`.
    pub(crate) unsafe fn report(&self, node: *mut c_void) {
        let bits = unsafe { node.cast::<u8>().add(Self::BITS_OFFSET).cast::<VkBool32>() };
        for index in 0..self.count {
            unsafe {
                bits.add(index)
                    .write(VkBool32::from(self.implemented.contains(&index)));
            }
        }
    }

    /// # Safety
    /// `node` must point to a readable struct identified by this entry's `sType`.
    pub(crate) unsafe fn first_unimplemented_request(
        &self,
        node: *const VkBaseInStructure,
    ) -> Option<usize> {
        let bits = unsafe { node.cast::<u8>().add(Self::BITS_OFFSET).cast::<VkBool32>() };
        (0..self.count).find(|&index| {
            let requested = unsafe { bits.add(index).read() };
            requested != VK_FALSE && !self.implemented.contains(&index)
        })
    }

    const BITS_OFFSET: usize = core::mem::size_of::<HeaderWithBits<0>>();
}

const fn feature(s_type: i32, name: &'static str, count: usize) -> FeatureStruct {
    FeatureStruct {
        s_type,
        name,
        count,
        implemented: &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::VK_TRUE;

    #[test]
    fn bool_payload_follows_stype_and_pnext() {
        assert_eq!(FeatureStruct::BITS_OFFSET, 16);
    }

    #[test]
    fn table_has_unique_stypes_and_only_backed_features_true() {
        let mut stypes = FeatureStruct::ALL
            .iter()
            .map(|feature| feature.s_type)
            .collect::<Vec<_>>();
        let original = stypes.len();
        stypes.sort_unstable();
        stypes.dedup();
        assert_eq!(stypes.len(), original);
        assert_eq!(
            FeatureStruct::ALL
                .iter()
                .map(|feature| feature.implemented.len())
                .sum::<usize>(),
            2
        );
    }

    #[test]
    fn rgba10x6_feature_query_and_device_request_agree() {
        let feature = FeatureStruct::matching(1_000_344_000).unwrap();
        assert_eq!(feature.name, "VkPhysicalDeviceRGBA10X6FormatsFeaturesEXT");

        let mut node = HeaderWithBits::<1> {
            s_type: feature.s_type,
            p_next: core::ptr::null_mut(),
            bits: [0xcdcd_cdcd],
        };
        let mut query = crate::types::VkPhysicalDeviceFeatures2 {
            s_type: crate::types::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
            p_next: &mut node as *mut _ as *mut c_void,
            features: crate::types::VkPhysicalDeviceFeatures {
                bits: [VK_FALSE; 55],
            },
        };
        crate::instance::vkGetPhysicalDeviceFeatures2(
            core::ptr::null_mut(),
            &mut query as *mut _ as *mut c_void,
        );
        assert_eq!(node.bits, [VK_TRUE]);
        assert_eq!(
            unsafe {
                feature.first_unimplemented_request(
                    &node as *const _ as *const VkBaseInStructure,
                )
            },
            None
        );
    }

    #[test]
    fn report_overwrites_poison_and_preserves_header() {
        let feature = FeatureStruct::matching(
            crate::types::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES,
        )
        .unwrap();
        let mut tail = HeaderWithBits::<2> {
            s_type: 77,
            p_next: core::ptr::null_mut(),
            bits: [3, 4],
        };
        let mut node = HeaderWithBits::<1> {
            s_type: feature.s_type,
            p_next: &mut tail as *mut _ as *mut c_void,
            bits: [0xcdcd_cdcd],
        };

        unsafe { feature.report(&mut node as *mut _ as *mut c_void) };

        assert_eq!(node.s_type, feature.s_type);
        assert_eq!(node.p_next, &mut tail as *mut _ as *mut c_void);
        assert_eq!(node.bits, [VK_TRUE]);
        assert_eq!(tail.bits, [3, 4]);
    }

    #[test]
    fn features2_overwrites_every_recognized_standalone_payload() {
        let mut nodes = FeatureStruct::ALL
            .iter()
            .map(|feature| HeaderWithBits::<20> {
                s_type: feature.s_type,
                p_next: core::ptr::null_mut(),
                bits: [0xcdcd_cdcd; 20],
            })
            .collect::<Vec<_>>();
        for index in 0..nodes.len().saturating_sub(1) {
            nodes[index].p_next = &mut nodes[index + 1] as *mut _ as *mut c_void;
        }
        let mut query = crate::types::VkPhysicalDeviceFeatures2 {
            s_type: crate::types::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2,
            p_next: nodes.as_mut_ptr() as *mut c_void,
            features: crate::types::VkPhysicalDeviceFeatures {
                bits: [0xcdcd_cdcd; 55],
            },
        };

        crate::instance::vkGetPhysicalDeviceFeatures2(
            core::ptr::null_mut(),
            &mut query as *mut _ as *mut c_void,
        );

        for (feature, node) in FeatureStruct::ALL.iter().zip(nodes) {
            assert_eq!(node.s_type, feature.s_type);
            for (index, bit) in node.bits[..feature.count].iter().copied().enumerate() {
                assert_eq!(
                    bit,
                    VkBool32::from(feature.implemented.contains(&index)),
                    "{} bit {index}",
                    feature.name
                );
            }
            assert!(
                node.bits[feature.count..]
                    .iter()
                    .all(|&bit| bit == 0xcdcd_cdcd)
            );
        }
    }

    #[test]
    fn false_standalone_feature_request_is_detected() {
        let feature = FeatureStruct::matching(1_000_161_001).unwrap();
        let mut node = HeaderWithBits::<20> {
            s_type: feature.s_type,
            p_next: core::ptr::null_mut(),
            bits: [VK_FALSE; 20],
        };
        node.bits[17] = VK_TRUE;

        assert_eq!(
            unsafe {
                feature.first_unimplemented_request(&node as *const _ as *const VkBaseInStructure)
            },
            Some(17)
        );
    }
}
