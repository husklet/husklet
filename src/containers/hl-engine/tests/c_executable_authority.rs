#![cfg(hl_retained_c)]
#![allow(unsafe_code)]

use std::{ffi::c_void, os::raw::c_char, ptr};

const STATUS_OK: i32 = 0;
const STATUS_INVALID_ARGUMENT: i32 = 1;
const STATUS_NOT_FOUND: i32 = 6;
const ENGINE_ABI: u32 = 5;
const FD_TRANSFER: u32 = 1;
const HANDLE_CWD: u64 = u64::MAX;
const FILE_READ: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct HostResult {
    status: i32,
    detail_domain: u32,
    value: u64,
    detail: u64,
}

type OpenRelative = unsafe extern "C" fn(*mut c_void, u64, *const c_char, usize, u32, u32, u32) -> HostResult;
type Close = unsafe extern "C" fn(*mut c_void, u64) -> HostResult;
type UnusedFileOperation = unsafe extern "C" fn();

#[repr(C)]
struct FileServices {
    abi: u32,
    size: u32,
    open_relative: Option<OpenRelative>,
    read_at: Option<UnusedFileOperation>,
    write_at: Option<UnusedFileOperation>,
    append: Option<UnusedFileOperation>,
    metadata: Option<UnusedFileOperation>,
    close: Option<Close>,
}

#[repr(C)]
struct HostServices {
    abi: u32,
    size: u32,
    capabilities: u64,
    context: *mut c_void,
    memory: *const c_void,
    clock: *const c_void,
    log: *const c_void,
    file: *const FileServices,
}

#[repr(C)]
#[derive(Debug)]
struct EngineExecutable {
    abi: u32,
    size: u32,
    ownership: u32,
    reserved: u32,
    host_handle: u64,
    image: *const c_void,
    image_size: usize,
}

unsafe extern "C" {
    fn hl_c_backend_executable_open(
        services: *const HostServices,
        host_path: *const c_char,
        output: *mut EngineExecutable,
    ) -> i32;
    fn hl_c_backend_executable_discard(services: *const HostServices, executable: *mut EngineExecutable);
}

// Keep the package library in this integration-test link. Its build script
// supplies the retained C archives used by the direct ABI assertions below.
fn link_engine_native_archives() {
    drop(hl_engine::options::Options::default());
}

struct State {
    open_result: HostResult,
    path: Vec<u8>,
    directory: u64,
    access: u32,
    creation: u32,
    permissions: u32,
    closes: Vec<u64>,
}

unsafe extern "C" fn open_relative(
    context: *mut c_void,
    directory: u64,
    path: *const c_char,
    path_size: usize,
    access: u32,
    creation: u32,
    permissions: u32,
) -> HostResult {
    // SAFETY: Every test points context at its live State and provides a path span
    // that remains valid for this synchronous callback.
    let state = unsafe { &mut *context.cast::<State>() };
    state.path = unsafe { std::slice::from_raw_parts(path.cast::<u8>(), path_size) }.to_vec();
    state.directory = directory;
    state.access = access;
    state.creation = creation;
    state.permissions = permissions;
    state.open_result
}

unsafe extern "C" fn close(context: *mut c_void, handle: u64) -> HostResult {
    // SAFETY: Every test points context at its live State.
    let state = unsafe { &mut *context.cast::<State>() };
    state.closes.push(handle);
    HostResult {
        status: STATUS_OK,
        detail_domain: 0,
        value: 0,
        detail: 0,
    }
}

fn services(state: &mut State, open: Option<OpenRelative>) -> (FileServices, HostServices) {
    let file = FileServices {
        abi: 0,
        size: std::mem::size_of::<FileServices>() as u32,
        open_relative: open,
        read_at: None,
        write_at: None,
        append: None,
        metadata: None,
        close: Some(close),
    };
    let host = HostServices {
        abi: 0,
        size: std::mem::size_of::<HostServices>() as u32,
        capabilities: 0,
        context: ptr::from_mut(state).cast(),
        memory: ptr::null(),
        clock: ptr::null(),
        log: ptr::null(),
        file: ptr::from_ref(&file),
    };
    (file, host)
}

fn executable_with_poison() -> EngineExecutable {
    EngineExecutable {
        abi: u32::MAX,
        size: u32::MAX,
        ownership: u32::MAX,
        reserved: u32::MAX,
        host_handle: u64::MAX - 1,
        image: ptr::dangling(),
        image_size: usize::MAX,
    }
}

#[test]
fn opens_workspace_path_as_transfer_authority_and_discards_on_create_failure() {
    link_engine_native_archives();
    let mut state = State {
        open_result: HostResult {
            status: STATUS_OK,
            detail_domain: 0,
            value: 41,
            detail: 0,
        },
        path: Vec::new(),
        directory: 0,
        access: 0,
        creation: u32::MAX,
        permissions: u32::MAX,
        closes: Vec::new(),
    };
    let (file, mut host) = services(&mut state, Some(open_relative));
    host.file = ptr::from_ref(&file);
    let path = b"/workspace/staged/guest\0";
    let mut executable = executable_with_poison();

    // SAFETY: host, path, and output remain live for the duration of both calls.
    let status = unsafe { hl_c_backend_executable_open(&raw const host, path.as_ptr().cast(), &raw mut executable) };
    assert_eq!(status, STATUS_OK);
    assert_eq!(state.path, b"/workspace/staged/guest");
    assert_eq!(state.directory, HANDLE_CWD);
    assert_eq!(state.access, FILE_READ);
    assert_eq!(state.creation, 0);
    assert_eq!(state.permissions, 0);
    assert_eq!(executable.abi, ENGINE_ABI);
    assert_eq!(executable.size as usize, std::mem::size_of::<EngineExecutable>());
    assert_eq!(executable.ownership, FD_TRANSFER);
    assert_eq!(executable.reserved, 0);
    assert_eq!(executable.host_handle, 41);
    assert!(executable.image.is_null());
    assert_eq!(executable.image_size, 0);

    // SAFETY: the authority has not been transferred to an engine.
    unsafe { hl_c_backend_executable_discard(&raw const host, &raw mut executable) };
    assert_eq!(state.closes, [41]);
    assert_eq!(executable.host_handle, 0);
    assert_eq!(executable.ownership, 0);
}

#[test]
fn failure_propagates_without_leaving_a_live_authority() {
    let mut state = State {
        open_result: HostResult {
            status: STATUS_NOT_FOUND,
            detail_domain: 0,
            value: 73,
            detail: 0,
        },
        path: Vec::new(),
        directory: 0,
        access: 0,
        creation: 0,
        permissions: 0,
        closes: Vec::new(),
    };
    let (file, mut host) = services(&mut state, Some(open_relative));
    host.file = ptr::from_ref(&file);
    let mut executable = executable_with_poison();

    // SAFETY: host, path, and output remain live for the call.
    let status = unsafe { hl_c_backend_executable_open(&raw const host, c"/missing".as_ptr(), &raw mut executable) };
    assert_eq!(status, STATUS_NOT_FOUND);
    assert_eq!(executable.host_handle, 0);
    assert_eq!(executable.ownership, 0);

    // SAFETY: a cleared authority is valid to discard and must be a no-op.
    unsafe { hl_c_backend_executable_discard(&raw const host, &raw mut executable) };
    assert!(state.closes.is_empty());
}

#[test]
fn invalid_inputs_are_rejected_before_opening() {
    let mut state = State {
        open_result: HostResult {
            status: STATUS_OK,
            detail_domain: 0,
            value: 9,
            detail: 0,
        },
        path: Vec::new(),
        directory: 0,
        access: 0,
        creation: 0,
        permissions: 0,
        closes: Vec::new(),
    };
    let (file, mut host) = services(&mut state, None);
    host.file = ptr::from_ref(&file);
    let mut executable = executable_with_poison();

    // SAFETY: host and output are live; the helper validates the missing callback.
    let status = unsafe { hl_c_backend_executable_open(&raw const host, c"/guest".as_ptr(), &raw mut executable) };
    assert_eq!(status, STATUS_INVALID_ARGUMENT);
    assert_eq!(executable.host_handle, 0);
    assert!(state.path.is_empty());
}
