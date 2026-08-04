use super::{ErrnoMapper, LinuxHost, abi};
use crate::native_host::{HostError, WatchEvent, WatchSyscalls};
use std::ffi::CStr;

const NONBLOCK: i32 = 0x800;
const CLOEXEC: i32 = 0x80000;
const IN_MODIFY: u32 = 2;
const IN_MOVED_FROM: u32 = 0x40;
const IN_MOVED_TO: u32 = 0x80;
const IN_CREATE: u32 = 0x100;
const IN_DELETE: u32 = 0x200;
const HEADER: usize = 16;

impl WatchSyscalls for LinuxHost {
    fn watch_create(&self) -> Result<i32, HostError> {
        // SAFETY: scalar flags only; successful result is an owned descriptor.
        let descriptor = unsafe { inotify_init1(NONBLOCK | CLOEXEC) };
        (descriptor >= 0).then_some(descriptor).ok_or_else(ErrnoMapper::current)
    }

    fn watch_add(&self, descriptor: i32, path: &CStr, interests: u32) -> Result<i32, HostError> {
        let mask = (if interests & 1 != 0 { IN_MODIFY } else { 0 })
            | (if interests & 2 != 0 { IN_CREATE } else { 0 })
            | (if interests & 4 != 0 { IN_DELETE } else { 0 })
            | (if interests & 8 != 0 {
                IN_MOVED_FROM | IN_MOVED_TO
            } else {
                0
            });
        if mask == 0 {
            return Err(HostError::Invalid);
        }
        // SAFETY: path is NUL-terminated and borrowed for the synchronous call.
        let watch = unsafe { inotify_add_watch(descriptor, path.as_ptr(), mask) };
        (watch >= 0).then_some(watch).ok_or_else(ErrnoMapper::current)
    }

    fn watch_remove(&self, descriptor: i32, watch: i32) -> Result<(), HostError> {
        // SAFETY: scalar descriptor and kernel-issued watch token.
        let result = unsafe { inotify_rm_watch(descriptor, watch) };
        (result == 0).then_some(()).ok_or_else(ErrnoMapper::current)
    }

    fn watch_read(&self, descriptor: i32, events: &mut Vec<WatchEvent>) -> Result<(), HostError> {
        let mut bytes = [0_u8; 16_384];
        // SAFETY: bytes is uniquely writable for its exact size.
        let count = unsafe { abi::read(descriptor, bytes.as_mut_ptr().cast(), bytes.len()) };
        let count: usize = count.try_into().map_err(|_| ErrnoMapper::current())?;
        WatchDecoder::decode(&bytes[..count], events)
    }
}

struct WatchDecoder;

impl WatchDecoder {
    fn decode(bytes: &[u8], output: &mut Vec<WatchEvent>) -> Result<(), HostError> {
        let mut offset = 0;
        while offset < bytes.len() {
            if bytes.len() - offset < HEADER {
                return Err(HostError::Failed);
            }
            let token = i32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let mask = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let cookie = u32::from_ne_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
            let length = u32::from_ne_bytes(bytes[offset + 12..offset + 16].try_into().unwrap()) as usize;
            let end = offset.checked_add(HEADER + length).ok_or(HostError::Failed)?;
            if end > bytes.len() {
                return Err(HostError::Failed);
            }
            let raw_name = &bytes[offset + HEADER..end];
            let name_length = raw_name.iter().position(|byte| *byte == 0).unwrap_or(raw_name.len());
            let interests = (if mask & IN_MODIFY != 0 { 1 } else { 0 })
                | (if mask & IN_CREATE != 0 { 2 } else { 0 })
                | (if mask & IN_DELETE != 0 { 4 } else { 0 })
                | (if mask & (IN_MOVED_FROM | IN_MOVED_TO) != 0 {
                    8
                } else {
                    0
                });
            output.push(WatchEvent::native(
                token,
                interests,
                cookie,
                raw_name[..name_length].to_vec(),
            ));
            offset = end;
        }
        Ok(())
    }
}

unsafe extern "C" {
    fn inotify_init1(flags: i32) -> i32;
    fn inotify_add_watch(descriptor: i32, path: *const core::ffi::c_char, mask: u32) -> i32;
    fn inotify_rm_watch(descriptor: i32, watch: i32) -> i32;
}
