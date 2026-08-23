#![allow(unsafe_code)]

#[cfg(unix)]
use std::ffi::c_int;
use std::{
    ffi::{c_char, c_void},
    path::Path,
};

pub(crate) struct DynamicLibrary(*mut c_void);

// SAFETY: the handle is immutable after construction, and symbol lookup is serialized by OnceLock.
unsafe impl Send for DynamicLibrary {}
// SAFETY: the platform loaders permit concurrent calls through resolved immutable function pointers.
unsafe impl Sync for DynamicLibrary {}

impl DynamicLibrary {
    pub(crate) fn build_fingerprint(&self) -> Result<String, String> {
        let address = self.symbol(b"hl_c_backend_build_fingerprint\0")?;
        // SAFETY: the named export is declared to return a static NUL-terminated string.
        let reader: unsafe extern "C" fn() -> *const c_char = unsafe { std::mem::transmute(address) };
        // SAFETY: the export's declaration guarantees a static NUL-terminated result.
        let value = unsafe { std::ffi::CStr::from_ptr(reader()) };
        Ok(String::from_utf8_lossy(value.to_bytes()).into_owned())
    }
}

#[cfg(unix)]
impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this handle was returned by dlopen and is dropped at most once.
            unsafe { dlclose(self.0) };
        }
    }
}

#[cfg(windows)]
impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this handle was returned by LoadLibraryExW and is dropped at most once.
            unsafe { FreeLibrary(self.0) };
        }
    }
}

#[cfg(unix)]
impl DynamicLibrary {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
        // SAFETY: path is NUL-terminated and flags request immediate, local binding.
        let handle = unsafe { dlopen(path.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            Err(dynamic_error())
        } else {
            Ok(Self(handle))
        }
    }

    pub(crate) fn symbol(&self, name: &'static [u8]) -> Result<*mut c_void, String> {
        // SAFETY: names are static NUL-terminated byte strings and the handle remains live.
        let address = unsafe { dlsym(self.0, name.as_ptr().cast()) };
        if address.is_null() {
            Err(dynamic_error())
        } else {
            Ok(address)
        }
    }
}

#[cfg(unix)]
fn dynamic_error() -> String {
    // SAFETY: dlerror returns either null or a thread-local NUL-terminated message.
    let message = unsafe { dlerror() };
    if message.is_null() {
        "dynamic loader did not report an error".to_owned()
    } else {
        // SAFETY: a non-null dlerror result is a NUL-terminated string valid until the next loader call.
        unsafe { std::ffi::CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(unix)]
const RTLD_LOCAL: c_int = 0;
#[cfg(unix)]
const RTLD_NOW: c_int = 2;

#[cfg(unix)]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn dlclose(handle: *mut c_void) -> c_int;
}

#[cfg(windows)]
impl DynamicLibrary {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt as _;
        let mut path = path.as_os_str().encode_wide().collect::<Vec<_>>();
        path.push(0);
        // SAFETY: the path is absolute and NUL-terminated; flags restrict dependency lookup to the DLL
        // directory and System32, never the current directory or ambient PATH.
        let handle = unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                std::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if handle.is_null() {
            Err(format!("Windows loader error {}", unsafe { GetLastError() }))
        } else {
            Ok(Self(handle))
        }
    }

    pub(crate) fn symbol(&self, name: &'static [u8]) -> Result<*mut c_void, String> {
        // SAFETY: the symbol name is static and NUL-terminated and the module remains live.
        let address = unsafe { GetProcAddress(self.0, name.as_ptr().cast()) };
        if address.is_null() {
            Err(format!("Windows loader error {}", unsafe { GetLastError() }))
        } else {
            Ok(address)
        }
    }
}

#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

#[cfg(windows)]
unsafe extern "system" {
    fn LoadLibraryExW(path: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn GetLastError() -> u32;
    fn FreeLibrary(module: *mut c_void) -> i32;
}
