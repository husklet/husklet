use ash::vk;

#[test]
fn robust_buffer_access_is_not_advertised_by_either_core_query() {
    let mut legacy = vk::PhysicalDeviceFeatures {
        robust_buffer_access: vk::TRUE,
        ..Default::default()
    };
    vk_hl::vkGetPhysicalDeviceFeatures(core::ptr::null_mut(), &mut legacy);
    assert_eq!(legacy.robust_buffer_access, vk::FALSE);

    let mut features2 = vk::PhysicalDeviceFeatures2::default();
    features2.features.robust_buffer_access = vk::TRUE;
    vk_hl::vkGetPhysicalDeviceFeatures2(core::ptr::null_mut(), &mut features2);
    assert_eq!(features2.features.robust_buffer_access, vk::FALSE);
}

#[test]
fn logical_device_rejects_request_for_unadvertised_robust_access() {
    let requested = vk::PhysicalDeviceFeatures {
        robust_buffer_access: vk::TRUE,
        ..Default::default()
    };
    let create_info = vk::DeviceCreateInfo {
        p_enabled_features: &requested,
        ..Default::default()
    };
    let physical_device = 1usize as vk_hl::types::VkPhysicalDevice;
    let mut device = 1usize as vk_hl::types::VkDevice;
    let result = vk_hl::vkCreateDevice(
        physical_device,
        &create_info,
        core::ptr::null(),
        &mut device,
    );
    assert_eq!(result, vk_hl::types::VK_ERROR_FEATURE_NOT_PRESENT);
    assert!(device.is_null(), "failed creation clears the output handle");
}
