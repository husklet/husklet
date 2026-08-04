#[repr(C)]
pub(crate) struct OpenHow {
    pub(crate) flags: u64,
    pub(crate) mode: u64,
    pub(crate) resolve: u64,
}
