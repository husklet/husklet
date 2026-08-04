//! Bounded typed protocol for one authority-owned filesystem root.

mod slot;
mod wire;

use slot::{Slot, Slots};
use wire::{APPEND, CLOSE, DENTS, LINK, OPEN, READ, STAT, TRUNCATE, WRITE, WireBytes};

pub use wire::Wire;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeStat {
    pub size: u64,
    pub mode: u32,
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeKind {
    File,
    Directory,
    Link,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeOpen {
    pub kind: TreeKind,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub append: bool,
    pub exclusive: bool,
    pub mode: u32,
}

impl TreeOpen {
    #[must_use]
    pub const fn read(kind: TreeKind) -> Self {
        Self {
            kind,
            read: true,
            write: false,
            create: false,
            truncate: false,
            append: false,
            exclusive: false,
            mode: 0,
        }
    }
}

pub trait TreeObject: Send {
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, i32>;
    fn stat(&self) -> Result<TreeStat, i32>;
    fn read_link(&self, maximum: usize) -> Result<Vec<u8>, i32>;
    fn entries(&mut self, maximum: usize) -> Result<Vec<u8>, i32>;
    fn write_at(&mut self, _: u64, _: &[u8]) -> Result<usize, i32> {
        Err(9)
    }
    fn append(&mut self, _: &[u8]) -> Result<(usize, u64), i32> {
        Err(9)
    }
    fn truncate(&mut self, _: u64) -> Result<(), i32> {
        Err(9)
    }
    /// Opens a relative path while retaining the authority root that produced
    /// this object. Absolute symlinks and parent traversal must remain confined
    /// to that root rather than escaping into the provider host namespace.
    fn open_in_root(&self, _: &[u8], _: TreeOpen) -> Result<Box<dyn TreeObject>, i32> {
        Err(95)
    }
}

/// Provider-owned capability for one pinned filesystem root.
///
/// Implementations resolve every path beneath the pinned root. In particular,
/// leading `/`, `..`, and absolute symlink targets name locations inside this
/// root and can never select the provider host's global filesystem root.
pub trait TreeRoot {
    fn open_in_root(&mut self, path: &[u8], options: TreeOpen) -> Result<Box<dyn TreeObject>, i32>;
}

pub struct TreeAuthority<B> {
    backend: B,
    slots: Slots,
    byte_limit: usize,
}

impl<B: TreeRoot> TreeAuthority<B> {
    pub fn new(backend: B, handles: usize, byte_limit: usize) -> Option<Self> {
        if handles == 0 || handles > u16::MAX as usize || byte_limit == 0 || byte_limit > Wire::MAX_DATA {
            return None;
        }
        let slots = (0..handles)
            .map(|_| Slot {
                generation: 0,
                object: None,
            })
            .collect();
        Some(Self {
            backend,
            slots: Slots(slots),
            byte_limit,
        })
    }

    pub fn dispatch(&mut self, request: &[u8]) -> Vec<u8> {
        let result = match request.first().copied() {
            Some(OPEN) => self.open(request),
            Some(READ) => self.read(request),
            Some(STAT) => self.stat(request),
            Some(LINK) => self.link(request),
            Some(DENTS) => self.dents(request),
            Some(CLOSE) => self.close(request),
            Some(WRITE) => self.write(request),
            Some(APPEND) => self.append(request),
            Some(TRUNCATE) => self.truncate(request),
            _ => Err(71),
        };
        result.unwrap_or_else(Wire::error)
    }

    fn open(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() < 18 {
            return Err(22);
        }
        let base = WireBytes::u64(request, 2)?;
        let flags = request[10];
        let mode = WireBytes::u32(request, 11)?;
        let length = usize::from(u16::from_le_bytes([request[15], request[16]]));
        if request[17] != 0
            || length == 0
            || length > 4096
            || request.len() != 18 + length
            || request[1] > 2
            || flags & !0x3f != 0
        {
            return Err(22);
        }
        let path = &request[18..];
        if path.contains(&0) || (base == 0) != (path[0] == b'/') {
            return Err(22);
        }
        let index = self.slots.0.iter().position(|slot| slot.object.is_none()).ok_or(24)?;
        let kind = match request[1] {
            0 => TreeKind::File,
            1 => TreeKind::Directory,
            _ => TreeKind::Link,
        };
        let options = TreeOpen {
            kind,
            read: flags & 1 != 0,
            write: flags & 2 != 0,
            create: flags & 4 != 0,
            truncate: flags & 8 != 0,
            append: flags & 16 != 0,
            exclusive: flags & 32 != 0,
            mode,
        };
        if (!options.read && !options.write)
            || kind != TreeKind::File
                && (options.write || options.create || options.truncate || options.append || options.exclusive)
        {
            return Err(22);
        }
        let object = if base == 0 {
            self.backend.open_in_root(path, options)?
        } else {
            self.slots.resolve(base)?.open_in_root(path, options)?
        };
        let slot = &mut self.slots.0[index];
        slot.generation = slot.generation.wrapping_add(1).max(1);
        slot.object = Some(object);
        let mut reply = vec![OPEN];
        WireBytes::put_u64(&mut reply, Slots::handle(index, slot.generation));
        Ok(reply)
    }

    fn read(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 21 {
            return Err(22);
        }
        let handle = WireBytes::u64(request, 1)?;
        let offset = WireBytes::u64(request, 9)?;
        let size = WireBytes::u32(request, 17)? as usize;
        if size > self.byte_limit {
            return Err(90);
        }
        offset.checked_add(size as u64).ok_or(22)?;
        let object = self.slots.resolve(handle)?;
        let mut bytes = vec![0; size];
        let count = object.read_at(offset, &mut bytes)?;
        if count > size {
            return Err(5);
        }
        bytes.truncate(count);
        let mut reply = vec![READ];
        WireBytes::put_u32(&mut reply, count as u32);
        reply.extend(bytes);
        Ok(reply)
    }

    fn stat(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 9 {
            return Err(22);
        }
        let value = self.slots.resolve(WireBytes::u64(request, 1)?)?.stat()?;
        let mut reply = vec![STAT];
        WireBytes::put_u64(&mut reply, value.size);
        WireBytes::put_u32(&mut reply, value.mode);
        WireBytes::put_u64(&mut reply, value.device);
        WireBytes::put_u64(&mut reply, value.inode);
        Ok(reply)
    }

    fn link(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        self.bytes(request, LINK, |object, size| object.read_link(size))
    }

    fn dents(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        self.bytes(request, DENTS, |object, size| object.entries(size))
    }

    fn bytes(
        &mut self,
        request: &[u8],
        kind: u8,
        action: impl FnOnce(&mut dyn TreeObject, usize) -> Result<Vec<u8>, i32>,
    ) -> Result<Vec<u8>, i32> {
        if request.len() != 13 {
            return Err(22);
        }
        let size = WireBytes::u32(request, 9)? as usize;
        if size > self.byte_limit {
            return Err(90);
        }
        let handle = WireBytes::u64(request, 1)?;
        let bytes = action(self.slots.resolve(handle)?.as_mut(), size)?;
        if bytes.len() > size {
            return Err(5);
        }
        let mut reply = vec![kind];
        WireBytes::put_u32(&mut reply, bytes.len() as u32);
        reply.extend(bytes);
        Ok(reply)
    }

    fn close(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 9 {
            return Err(22);
        }
        let handle = WireBytes::u64(request, 1)?;
        self.slots.resolve_slot(handle)?.object = None;
        Ok(vec![CLOSE])
    }

    fn write(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() < 21 {
            return Err(22);
        }
        let handle = WireBytes::u64(request, 1)?;
        let offset = WireBytes::u64(request, 9)?;
        let size = WireBytes::u32(request, 17)? as usize;
        if size > self.byte_limit.min(Wire::MAX_WRITE_DATA) || request.len() != 21 + size {
            return Err(90);
        }
        offset.checked_add(size as u64).ok_or(22)?;
        let count = self.slots.resolve(handle)?.write_at(offset, &request[21..])?;
        Self::count_reply(WRITE, count, size)
    }

    fn append(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() < 13 {
            return Err(22);
        }
        let handle = WireBytes::u64(request, 1)?;
        let size = WireBytes::u32(request, 9)? as usize;
        if size > self.byte_limit.min(Wire::MAX_APPEND_DATA) || request.len() != 13 + size {
            return Err(90);
        }
        let (count, end) = self.slots.resolve(handle)?.append(&request[13..])?;
        if count > size {
            return Err(5);
        }
        let mut reply = vec![APPEND];
        WireBytes::put_u32(&mut reply, count as u32);
        WireBytes::put_u64(&mut reply, end);
        Ok(reply)
    }

    fn truncate(&mut self, request: &[u8]) -> Result<Vec<u8>, i32> {
        if request.len() != 17 {
            return Err(22);
        }
        let handle = WireBytes::u64(request, 1)?;
        self.slots.resolve(handle)?.truncate(WireBytes::u64(request, 9)?)?;
        Ok(vec![TRUNCATE])
    }

    fn count_reply(kind: u8, count: usize, maximum: usize) -> Result<Vec<u8>, i32> {
        if count > maximum {
            return Err(5);
        }
        let mut reply = vec![kind];
        WireBytes::put_u32(&mut reply, count as u32);
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::TreeWire;

    struct Backend;
    struct Object(Vec<u8>);
    impl TreeRoot for Backend {
        fn open_in_root(&mut self, path: &[u8], _: TreeOpen) -> Result<Box<dyn TreeObject>, i32> {
            if path.windows(2).any(|part| part == b"..") {
                return Err(13);
            }
            Ok(Box::new(Object(b"pinned".to_vec())))
        }
    }
    impl TreeObject for Object {
        fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, i32> {
            let offset = usize::try_from(offset).map_err(|_| 22)?;
            let bytes = self.0.get(offset..).unwrap_or_default();
            let size = bytes.len().min(output.len());
            output[..size].copy_from_slice(&bytes[..size]);
            Ok(size)
        }
        fn stat(&self) -> Result<TreeStat, i32> {
            Ok(TreeStat {
                size: 6,
                mode: 0o100444,
                device: 1,
                inode: 2,
            })
        }
        fn read_link(&self, _: usize) -> Result<Vec<u8>, i32> {
            Err(22)
        }
        fn entries(&mut self, maximum: usize) -> Result<Vec<u8>, i32> {
            Ok(b"entry\0"[..maximum.min(6)].to_vec())
        }
        fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<usize, i32> {
            let offset = usize::try_from(offset).map_err(|_| 22)?;
            let end = offset.checked_add(input.len()).ok_or(22)?;
            if self.0.len() < end {
                self.0.resize(end, 0);
            }
            self.0[offset..end].copy_from_slice(input);
            Ok(input.len())
        }
        fn append(&mut self, input: &[u8]) -> Result<(usize, u64), i32> {
            self.0.extend(input);
            Ok((input.len(), self.0.len() as u64))
        }
        fn truncate(&mut self, size: u64) -> Result<(), i32> {
            self.0.resize(usize::try_from(size).map_err(|_| 22)?, 0);
            Ok(())
        }
    }

    #[test]
    fn bounded_handles() {
        let mut server = TreeAuthority::new(Backend, 1, 8).unwrap();
        let handle = TreeWire::open_reply(&server.dispatch(&TreeWire::open(b"/file", false).unwrap())).unwrap();
        assert_eq!(
            TreeWire::data_reply(&server.dispatch(&TreeWire::read(handle, 0, 6).unwrap()), READ, 6).unwrap(),
            b"pinned"
        );
        assert_eq!(
            TreeWire::stat_reply(&server.dispatch(&TreeWire::stat(handle)))
                .unwrap()
                .inode,
            2
        );
        assert_eq!(
            TreeWire::open_reply(&server.dispatch(&TreeWire::open(b"/other", false).unwrap())),
            Err(24)
        );
        TreeWire::close_reply(&server.dispatch(&TreeWire::close(handle))).unwrap();
        assert_eq!(TreeWire::stat_reply(&server.dispatch(&TreeWire::stat(handle))), Err(9));
    }

    #[test]
    fn rejects_bad_input() {
        let mut server = TreeAuthority::new(Backend, 2, 4).unwrap();
        assert_eq!(
            TreeWire::open_reply(&server.dispatch(&TreeWire::open(b"/../escape", false).unwrap())),
            Err(13)
        );
        let handle = TreeWire::open_reply(&server.dispatch(&TreeWire::open(b"/file", false).unwrap())).unwrap();
        assert_eq!(
            TreeWire::data_reply(&server.dispatch(&TreeWire::read(handle, 0, 5).unwrap()), READ, 5),
            Err(90)
        );
        let mut appended = TreeWire::open(b"/file", false).unwrap();
        appended.push(0);
        assert_eq!(TreeWire::open_reply(&server.dispatch(&appended)), Err(22));
    }

    #[test]
    fn bounded_mutation() {
        let mut server = TreeAuthority::new(Backend, 1, 8).unwrap();
        let options = TreeOpen {
            kind: TreeKind::File,
            read: true,
            write: true,
            create: false,
            truncate: false,
            append: false,
            exclusive: false,
            mode: 0,
        };
        let handle =
            TreeWire::open_reply(&server.dispatch(&TreeWire::open_options(0, b"/file", options).unwrap())).unwrap();
        assert_eq!(
            TreeWire::write_reply(&server.dispatch(&TreeWire::write(handle, 1, b"XY").unwrap()), 2),
            Ok(2)
        );
        assert_eq!(
            TreeWire::append_reply(&server.dispatch(&TreeWire::append(handle, b"!").unwrap()), 1),
            Ok((1, 7))
        );
        assert_eq!(
            TreeWire::truncate_reply(&server.dispatch(&TreeWire::truncate(handle, 3))),
            Ok(())
        );
        assert_eq!(
            TreeWire::read_reply(&server.dispatch(&TreeWire::read(handle, 0, 8).unwrap()), 8).unwrap(),
            b"pXY"
        );
    }
}
