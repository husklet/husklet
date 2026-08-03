use crate::types::VkQueueFamilyProperties;
use core::ffi::c_void;

#[test]
fn queue_families_include_an_exclusive_compute_lane_and_truncate_to_capacity() {
    let mut count = 0;
    crate::instance::vkGetPhysicalDeviceQueueFamilyProperties(
        core::ptr::null_mut(),
        &mut count,
        core::ptr::null_mut(),
    );
    assert_eq!(count, 2);

    let mut families: [VkQueueFamilyProperties; 2] = unsafe { core::mem::zeroed() };
    crate::instance::vkGetPhysicalDeviceQueueFamilyProperties(
        core::ptr::null_mut(),
        &mut count,
        families.as_mut_ptr() as *mut c_void,
    );
    assert_eq!(count, 2);
    assert_ne!(families[0].queue_flags & 0x1, 0, "family 0 remains graphics-capable");
    assert_eq!(families[1].queue_flags & 0x1, 0, "family 1 is exclusive of graphics");
    assert_ne!(families[1].queue_flags & 0x2, 0, "family 1 serves compute");

    let mut one: VkQueueFamilyProperties = unsafe { core::mem::zeroed() };
    count = 1;
    crate::instance::vkGetPhysicalDeviceQueueFamilyProperties(
        core::ptr::null_mut(),
        &mut count,
        &mut one as *mut _ as *mut c_void,
    );
    assert_eq!(count, 1);
    assert_eq!(one.queue_flags, families[0].queue_flags);
}
