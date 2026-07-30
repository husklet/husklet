use core::ffi::c_void;

use super::support::DynamicState;

pub extern "C" fn vkCmdSetRasterizationSamplesEXT(
    command_buffer: *mut c_void,
    rasterization_samples: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.rasterization_samples = rasterization_samples
    });
}

pub extern "C" fn vkCmdSetSampleMaskEXT(
    command_buffer: *mut c_void,
    _samples: u32,
    p_sample_mask: *const c_void,
) {
    let mask = if p_sample_mask.is_null() {
        0
    } else {
        unsafe { *(p_sample_mask as *const u32) }
    };
    DynamicState::record(command_buffer, |ds| ds.sample_mask = mask);
}

pub extern "C" fn vkCmdSetAlphaToCoverageEnableEXT(
    command_buffer: *mut c_void,
    alpha_to_coverage_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.alpha_to_coverage_enable = alpha_to_coverage_enable != 0
    });
}

pub extern "C" fn vkCmdSetAlphaToOneEnableEXT(
    command_buffer: *mut c_void,
    alpha_to_one_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.alpha_to_one_enable = alpha_to_one_enable != 0
    });
}

pub extern "C" fn vkCmdSetSampleLocationsEnableEXT(
    command_buffer: *mut c_void,
    sample_locations_enable: u32,
) {
    DynamicState::record(command_buffer, |ds| {
        ds.sample_locations_enable = sample_locations_enable != 0
    });
}
