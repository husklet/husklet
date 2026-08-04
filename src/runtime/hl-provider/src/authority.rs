//! Bounded server side of the typed projected-file protocol.

const OPEN: u8 = 1;
const READ: u8 = 2;
const INFO: u8 = 3;
const CLOSE: u8 = 7;
const ERROR: u8 = 0xff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerLimits {
    pub handles: usize,
    pub read_bytes: usize,
}

impl ServerLimits {
    pub const fn new(handles: usize, read_bytes: usize) -> Option<Self> {
        if handles == 0 || handles > u16::MAX as usize || read_bytes == 0 || read_bytes > 65_536 {
            None
        } else {
            Some(Self { handles, read_bytes })
        }
    }
}

pub trait FileObject: Send {
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, i32>;
    fn info(&self) -> Result<FileInfo, i32> {
        Err(libc_errno::ENOSYS)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileInfo {
    pub size: u64,
    pub mode: u32,
    pub device: u64,
    pub inode: u64,
}

pub trait FileBackend {
    fn open(&mut self, service: u64, access: u8) -> Result<Box<dyn FileObject>, i32>;
}

struct Slot {
    generation: u16,
    file: Option<Box<dyn FileObject>>,
}

pub struct FileAuthority<B> {
    backend: B,
    slots: Vec<Slot>,
    read_limit: usize,
}

impl<B: FileBackend> FileAuthority<B> {
    pub fn new(backend: B, limits: ServerLimits) -> Self {
        let slots = (0..limits.handles)
            .map(|_| Slot {
                generation: 0,
                file: None,
            })
            .collect();
        Self {
            backend,
            slots,
            read_limit: limits.read_bytes,
        }
    }

    #[must_use]
    pub fn dispatch(&mut self, request: &[u8]) -> Vec<u8> {
        match request.first().copied() {
            Some(OPEN) => self.open(request).unwrap_or_else(Self::error),
            Some(READ) => self.read(request).unwrap_or_else(Self::error),
            Some(INFO) => self.info(request).unwrap_or_else(Self::error),
            Some(CLOSE) => self.close(request).unwrap_or_else(Self::error),
            _ => Self::error(libc_errno::EPROTO),
        }
    }

    fn open(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 10 || !(1..=3).contains(&request[9]) {
            return Err(libc_errno::EINVAL);
        }
        let service = WireBytes::u64(request, 1)?;
        if service == 0 {
            return Err(libc_errno::EINVAL);
        }
        let index = self
            .slots
            .iter()
            .position(|slot| slot.file.is_none())
            .ok_or(libc_errno::EMFILE)?;
        let file = self.backend.open(service, request[9])?;
        let slot = &mut self.slots[index];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        slot.file = Some(file);
        let mut reply = vec![OPEN];
        WireBytes::put_u64(&mut reply, Self::handle(index, slot.generation));
        Ok(reply)
    }

    fn read(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 21 {
            return Err(libc_errno::EINVAL);
        }
        let handle = WireBytes::u64(request, 1)?;
        let offset = WireBytes::u64(request, 9)?;
        let size = WireBytes::u32(request, 17)? as usize;
        if size > self.read_limit {
            return Err(libc_errno::EMSGSIZE);
        }
        offset.checked_add(size as u64).ok_or(libc_errno::EINVAL)?;
        let file = Self::resolve(&mut self.slots, handle)?;
        let mut bytes = vec![0_u8; size];
        let count = file.read_at(offset, &mut bytes)?;
        if count > size {
            return Err(libc_errno::EIO);
        }
        bytes.truncate(count);
        let mut reply = Vec::with_capacity(5 + count);
        reply.push(READ);
        WireBytes::put_u32(&mut reply, count as u32);
        reply.extend_from_slice(&bytes);
        Ok(reply)
    }

    fn info(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 9 {
            return Err(libc_errno::EINVAL);
        }
        let info = Self::resolve(&mut self.slots, WireBytes::u64(request, 1)?)?.info()?;
        let mut reply = vec![INFO];
        WireBytes::put_u64(&mut reply, info.size);
        WireBytes::put_u32(&mut reply, info.mode);
        WireBytes::put_u64(&mut reply, info.device);
        WireBytes::put_u64(&mut reply, info.inode);
        Ok(reply)
    }

    fn close(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 9 {
            return Err(libc_errno::EINVAL);
        }
        let handle = WireBytes::u64(request, 1)?;
        let (index, generation) = Self::decode(handle)?;
        let slot = self.slots.get_mut(index).ok_or(libc_errno::EBADF)?;
        if slot.generation != generation || slot.file.take().is_none() {
            return Err(libc_errno::EBADF);
        }
        Ok(vec![CLOSE])
    }

    fn resolve(slots: &mut [Slot], handle: u64) -> Result<&mut (dyn FileObject + 'static), i32> {
        let (index, generation) = Self::decode(handle)?;
        let slot = slots.get_mut(index).ok_or(libc_errno::EBADF)?;
        if slot.generation != generation {
            return Err(libc_errno::EBADF);
        }
        slot.file.as_deref_mut().ok_or(libc_errno::EBADF)
    }

    const fn handle(index: usize, generation: u16) -> u64 {
        ((generation as u64) << 32) | index as u64 + 1
    }

    fn decode(handle: u64) -> Result<(usize, u16), i32> {
        let raw = handle as u32;
        let generation = (handle >> 32) as u16;
        if raw == 0 || generation == 0 || handle >> 48 != 0 {
            return Err(libc_errno::EBADF);
        }
        Ok((raw as usize - 1, generation))
    }

    fn error(errno: i32) -> Vec<u8> {
        let errno = if (1..=4095).contains(&errno) {
            errno
        } else {
            libc_errno::EIO
        };
        let mut reply = vec![ERROR];
        reply.extend_from_slice(&errno.to_le_bytes());
        reply.extend_from_slice(&[0, 0]);
        reply
    }
}

pub struct FileWire;

impl FileWire {
    /// Maximum file bytes in one 4096-byte authenticated session payload.
    /// A read reply reserves one operation byte and one u32 length.
    pub const MAX_READ_DATA: usize = 4096 - 5;

    pub fn open(service: u64, access: u8) -> Vec<u8> {
        let mut request = vec![OPEN];
        WireBytes::put_u64(&mut request, service);
        request.push(access);
        request
    }

    pub fn open_reply(reply: &[u8]) -> Result<u64, i32> {
        Self::success(reply)?;
        if reply.len() != 9 || reply[0] != OPEN {
            return Err(libc_errno::EPROTO);
        }
        WireBytes::u64(reply, 1)
    }

    pub fn read(handle: u64, offset: u64, size: usize) -> Result<Vec<u8>, i32> {
        let size = u32::try_from(size).map_err(|_| libc_errno::EMSGSIZE)?;
        let mut request = vec![READ];
        WireBytes::put_u64(&mut request, handle);
        WireBytes::put_u64(&mut request, offset);
        WireBytes::put_u32(&mut request, size);
        Ok(request)
    }

    pub fn read_reply(reply: &[u8], maximum: usize) -> Result<Vec<u8>, i32> {
        Self::success(reply)?;
        if reply.len() < 5 || reply[0] != READ {
            return Err(libc_errno::EPROTO);
        }
        let size = WireBytes::u32(reply, 1)? as usize;
        if size > maximum || reply.len() != 5 + size {
            return Err(libc_errno::EPROTO);
        }
        Ok(reply[5..].to_vec())
    }

    pub fn close(handle: u64) -> Vec<u8> {
        let mut request = vec![CLOSE];
        WireBytes::put_u64(&mut request, handle);
        request
    }

    pub fn info(handle: u64) -> Vec<u8> {
        let mut request = vec![INFO];
        WireBytes::put_u64(&mut request, handle);
        request
    }

    pub fn info_reply(reply: &[u8]) -> Result<FileInfo, i32> {
        Self::success(reply)?;
        if reply.len() != 29 || reply[0] != INFO {
            return Err(libc_errno::EPROTO);
        }
        Ok(FileInfo {
            size: WireBytes::u64(reply, 1)?,
            mode: WireBytes::u32(reply, 9)?,
            device: WireBytes::u64(reply, 13)?,
            inode: WireBytes::u64(reply, 21)?,
        })
    }

    pub fn close_reply(reply: &[u8]) -> Result<(), i32> {
        Self::success(reply)?;
        (reply == [CLOSE]).then_some(()).ok_or(libc_errno::EPROTO)
    }

    fn success(reply: &[u8]) -> Result<(), i32> {
        if reply.len() == 7 && reply[0] == ERROR {
            let errno = i32::from_le_bytes(reply[1..5].try_into().unwrap());
            if reply[5..] == [0, 0] && (1..=4095).contains(&errno) {
                return Err(errno);
            }
            return Err(libc_errno::EPROTO);
        }
        Ok(())
    }
}

const _: () = assert!(FileWire::MAX_READ_DATA + 5 == 4096);

struct WireBytes;

impl WireBytes {
    fn put_u32(output: &mut Vec<u8>, value: u32) {
        output.extend_from_slice(&value.to_le_bytes());
    }
    fn put_u64(output: &mut Vec<u8>, value: u64) {
        output.extend_from_slice(&value.to_le_bytes());
    }
    fn u32(input: &[u8], offset: usize) -> Result<u32, i32> {
        input
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or(libc_errno::EPROTO)
    }
    fn u64(input: &[u8], offset: usize) -> Result<u64, i32> {
        input
            .get(offset..offset + 8)
            .and_then(|value| value.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or(libc_errno::EPROTO)
    }
}

mod libc_errno {
    pub const EIO: i32 = 5;
    pub const EBADF: i32 = 9;
    pub const EINVAL: i32 = 22;
    pub const EMFILE: i32 = 24;
    pub const ENOSYS: i32 = 38;
    pub const EPROTO: i32 = 71;
    pub const EMSGSIZE: i32 = 90;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Backend(Arc<AtomicUsize>);
    struct File(Arc<AtomicUsize>);

    impl Drop for File {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl FileObject for File {
        fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, i32> {
            let bytes = b"authority";
            let offset = usize::try_from(offset).map_err(|_| 22)?;
            if offset >= bytes.len() {
                return Ok(0);
            }
            let count = output.len().min(bytes.len() - offset);
            output[..count].copy_from_slice(&bytes[offset..offset + count]);
            Ok(count)
        }
    }
    impl FileBackend for Backend {
        fn open(&mut self, service: u64, access: u8) -> Result<Box<dyn FileObject>, i32> {
            if service != 1 || access != 1 {
                return Err(13);
            }
            Ok(Box::new(File(Arc::clone(&self.0))))
        }
    }

    #[test]
    fn lifecycle() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut server = FileAuthority::new(Backend(Arc::clone(&drops)), ServerLimits::new(1, 8).unwrap());
        let handle = FileWire::open_reply(&server.dispatch(&FileWire::open(1, 1))).unwrap();
        let reply = server.dispatch(&FileWire::read(handle, 1, 8).unwrap());
        assert_eq!(FileWire::read_reply(&reply, 8).unwrap(), b"uthority");
        FileWire::close_reply(&server.dispatch(&FileWire::close(handle))).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            FileWire::read_reply(&server.dispatch(&FileWire::read(handle, 0, 1).unwrap()), 1),
            Err(9)
        );
    }

    #[test]
    fn bounds_cleanup() {
        let drops = Arc::new(AtomicUsize::new(0));
        {
            let mut server = FileAuthority::new(Backend(Arc::clone(&drops)), ServerLimits::new(1, 4).unwrap());
            let handle = FileWire::open_reply(&server.dispatch(&FileWire::open(1, 1))).unwrap();
            assert_eq!(
                FileWire::read_reply(&server.dispatch(&FileWire::read(handle, 0, 5).unwrap()), 5),
                Err(90)
            );
            assert_eq!(FileWire::open_reply(&server.dispatch(&FileWire::open(1, 1))), Err(24));
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn adversarial_requests() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut server = FileAuthority::new(Backend(drops), ServerLimits::new(1, 4).unwrap());
        assert_eq!(FileWire::open_reply(&server.dispatch(&FileWire::open(1, 2))), Err(13));
        let mut traversal = FileWire::open(1, 1);
        traversal.extend_from_slice(b"/../../etc/passwd");
        assert_eq!(FileWire::open_reply(&server.dispatch(&traversal)), Err(22));
        let handle = FileWire::open_reply(&server.dispatch(&FileWire::open(1, 1))).unwrap();
        FileWire::close_reply(&server.dispatch(&FileWire::close(handle))).unwrap();
        let replacement = FileWire::open_reply(&server.dispatch(&FileWire::open(1, 1))).unwrap();
        assert_ne!(handle, replacement);
        assert_eq!(
            FileWire::read_reply(&server.dispatch(&FileWire::read(handle, 0, 1).unwrap()), 1),
            Err(9)
        );
    }
}
