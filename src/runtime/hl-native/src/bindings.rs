#![allow(unsafe_code)]

use std::ffi::{c_char, c_uint};

unsafe extern "C" {
    pub(super) fn hl_engine_abi() -> c_uint;
    pub(super) fn hl_engine_version() -> *const c_char;
}
