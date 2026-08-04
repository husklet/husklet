pub(super) mod mode {
    pub const TYPE_MASK: u32 = 0o170000;
    pub const FIFO: u32 = 0o010000;
    pub const CHARACTER: u32 = 0o020000;
    pub const DIRECTORY: u32 = 0o040000;
    pub const BLOCK: u32 = 0o060000;
    pub const REGULAR: u32 = 0o100000;
    pub const SYMLINK: u32 = 0o120000;
    pub const SOCKET: u32 = 0o140000;
}

#[cfg(target_os = "linux")]
pub(super) const LOOP: i32 = 40;
#[cfg(target_os = "macos")]
pub(super) const LOOP: i32 = 62;

#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(super) mod syscall {
    // Linux asm-generic and x86-64 assigned fchmodat2 the same stable number.
    pub const FCHMODAT2: libc::c_long = 452;
}
