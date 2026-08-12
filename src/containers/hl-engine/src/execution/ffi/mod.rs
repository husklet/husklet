mod executor;
mod image_plan;

pub(super) use image_plan::{CMainImagePlan, c_main_image_plan};

#[cfg(test)]
pub(super) use image_plan::{
    CAddressProjection, hl_native_address_projection_guest, hl_native_address_projection_init,
    hl_native_address_projection_init_elf, hl_native_address_projection_storage,
};
