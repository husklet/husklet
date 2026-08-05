pub(super) mod mode {
    pub const TYPE_MASK: u32 = 0o170_000;
    pub const FIFO: u32 = 0o010_000;
    pub const CHARACTER: u32 = 0o020_000;
    pub const DIRECTORY: u32 = 0o040_000;
    pub const BLOCK: u32 = 0o060_000;
    pub const REGULAR: u32 = 0o100_000;
    pub const SYMLINK: u32 = 0o120_000;
    pub const SOCKET: u32 = 0o140_000;
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
