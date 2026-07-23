use super::*;

/// `vkGetPhysicalDeviceProperties2` fills the maintenance4 pNext node with a real `maxBufferSize`
/// (2 GiB) and preserves the chain. wgpu-hal (Vulkan 1.3+) reads `max_buffer_size` from here; a
/// zero-initialized node (an unfilled branch) made it reject our device with
/// "Limit 'max_buffer_size' value … is better than allowed 0" and fall back to llvmpipe.
#[test]
fn properties2_fills_maintenance4_max_buffer_size_and_preserves_chain() {
    // A maintenance4 node the app zero-inits and chains after a sentinel tail node.
    let mut tail = VkBaseOutStructure {
        s_type: 0x7FFF_0001,
        p_next: core::ptr::null_mut(),
    };
    let mut m4 = VkPhysicalDeviceMaintenance4Properties {
        s_type: VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES,
        p_next: &mut tail as *mut _ as *mut c_void,
        max_buffer_size: 0,
    };
    // SAFETY: base VkPhysicalDeviceProperties is written by the entry point; zero-init is a valid
    // starting state for the C ABI struct the loader would hand us.
    let mut props2: VkPhysicalDeviceProperties2 = unsafe { core::mem::zeroed() };
    props2.p_next = &mut m4 as *mut _ as *mut c_void;

    vkGetPhysicalDeviceProperties2(core::ptr::null_mut(), &mut props2 as *mut _ as *mut c_void);

    assert_eq!(
        m4.max_buffer_size,
        1 << 31,
        "maintenance4 maxBufferSize must be 2 GiB"
    );
    assert_eq!(
        m4.s_type, VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_4_PROPERTIES,
        "sType intact"
    );
    // The chain past the filled node is preserved untouched.
    assert_eq!(
        m4.p_next, &mut tail as *mut _ as *mut c_void,
        "pNext chaining preserved"
    );
    assert_eq!(tail.s_type, 0x7FFF_0001, "downstream node untouched");
}
