use super::{TreeKind, TreeOpen, TreeStat};

pub(super) const OPEN: u8 = 16;
pub(super) const READ: u8 = 17;
pub(super) const STAT: u8 = 18;
pub(super) const LINK: u8 = 19;
pub(super) const DENTS: u8 = 20;
pub(super) const CLOSE: u8 = 21;
pub(super) const WRITE: u8 = 22;
pub(super) const APPEND: u8 = 23;
pub(super) const TRUNCATE: u8 = 24;
const ERROR: u8 = 0xff;

pub struct Wire;

impl Wire {
    pub const MAX_DATA: usize = 4096 - 5;
    pub const MAX_WRITE_DATA: usize = 4096 - 21;
    pub const MAX_APPEND_DATA: usize = 4096 - 13;

    #[must_use]
    pub fn is_request(request: &[u8]) -> bool {
        request.first().is_some_and(|kind| (OPEN..=TRUNCATE).contains(kind))
    }

    pub fn open(path: &[u8], directory: bool) -> Result<Vec<u8>, i32> {
        Self::open_options(
            0,
            path,
            TreeOpen::read(if directory { TreeKind::Directory } else { TreeKind::File }),
        )
    }

    pub fn open_at(base: u64, path: &[u8], directory: bool) -> Result<Vec<u8>, i32> {
        Self::open_options(
            base,
            path,
            TreeOpen::read(if directory { TreeKind::Directory } else { TreeKind::File }),
        )
    }

    pub fn open_link(path: &[u8]) -> Result<Vec<u8>, i32> {
        Self::open_options(0, path, TreeOpen::read(TreeKind::Link))
    }

    pub fn open_link_at(base: u64, path: &[u8]) -> Result<Vec<u8>, i32> {
        Self::open_options(base, path, TreeOpen::read(TreeKind::Link))
    }

    pub fn open_options(base: u64, path: &[u8], options: TreeOpen) -> Result<Vec<u8>, i32> {
        let length = u16::try_from(path.len()).map_err(|_| 36)?;
        let kind = match options.kind {
            TreeKind::File => 0,
            TreeKind::Directory => 1,
            TreeKind::Link => 2,
        };
        let flags = u8::from(options.read)
            | u8::from(options.write) << 1
            | u8::from(options.create) << 2
            | u8::from(options.truncate) << 3
            | u8::from(options.append) << 4
            | u8::from(options.exclusive) << 5;
        let mut value = vec![OPEN, kind];
        WireBytes::put_u64(&mut value, base);
        value.push(flags);
        WireBytes::put_u32(&mut value, options.mode);
        value.extend(length.to_le_bytes());
        value.push(0);
        value.extend(path);
        Ok(value)
    }

    pub fn open_reply(reply: &[u8]) -> Result<u64, i32> {
        Self::success(reply)?;
        if reply.len() != 9 || reply[0] != OPEN {
            return Err(71);
        }
        WireBytes::u64(reply, 1)
    }

    pub fn read(handle: u64, offset: u64, size: usize) -> Result<Vec<u8>, i32> {
        let size = u32::try_from(size).map_err(|_| 90)?;
        let mut value = vec![READ];
        WireBytes::put_u64(&mut value, handle);
        WireBytes::put_u64(&mut value, offset);
        WireBytes::put_u32(&mut value, size);
        Ok(value)
    }

    pub fn read_reply(reply: &[u8], maximum: usize) -> Result<Vec<u8>, i32> {
        Self::data_reply(reply, READ, maximum)
    }

    #[must_use]
    pub fn stat(handle: u64) -> Vec<u8> {
        Self::handle_request(STAT, handle)
    }

    pub fn link(handle: u64, size: usize) -> Result<Vec<u8>, i32> {
        Self::byte_request(LINK, handle, size)
    }

    pub fn dents(handle: u64, size: usize) -> Result<Vec<u8>, i32> {
        Self::byte_request(DENTS, handle, size)
    }

    pub fn link_reply(reply: &[u8], maximum: usize) -> Result<Vec<u8>, i32> {
        Self::data_reply(reply, LINK, maximum)
    }

    pub fn dents_reply(reply: &[u8], maximum: usize) -> Result<Vec<u8>, i32> {
        Self::data_reply(reply, DENTS, maximum)
    }

    #[must_use]
    pub fn close(handle: u64) -> Vec<u8> {
        Self::handle_request(CLOSE, handle)
    }

    pub fn write(handle: u64, offset: u64, input: &[u8]) -> Result<Vec<u8>, i32> {
        if input.len() > Self::MAX_WRITE_DATA {
            return Err(90);
        }
        let size = input.len() as u32;
        let mut value = Self::handle_request(WRITE, handle);
        WireBytes::put_u64(&mut value, offset);
        WireBytes::put_u32(&mut value, size);
        value.extend(input);
        Ok(value)
    }

    pub fn write_reply(reply: &[u8], maximum: usize) -> Result<usize, i32> {
        Self::count(reply, WRITE, maximum)
    }

    pub fn append(handle: u64, input: &[u8]) -> Result<Vec<u8>, i32> {
        if input.len() > Self::MAX_APPEND_DATA {
            return Err(90);
        }
        let size = input.len() as u32;
        let mut value = Self::handle_request(APPEND, handle);
        WireBytes::put_u32(&mut value, size);
        value.extend(input);
        Ok(value)
    }

    pub fn append_reply(reply: &[u8], maximum: usize) -> Result<(usize, u64), i32> {
        Self::success(reply)?;
        if reply.len() != 13 || reply[0] != APPEND {
            return Err(71);
        }
        let count = WireBytes::u32(reply, 1)? as usize;
        if count > maximum {
            return Err(71);
        }
        Ok((count, WireBytes::u64(reply, 5)?))
    }

    #[must_use]
    pub fn truncate(handle: u64, size: u64) -> Vec<u8> {
        let mut value = Self::handle_request(TRUNCATE, handle);
        WireBytes::put_u64(&mut value, size);
        value
    }

    pub fn truncate_reply(reply: &[u8]) -> Result<(), i32> {
        Self::success(reply)?;
        (reply == [TRUNCATE]).then_some(()).ok_or(71)
    }

    pub fn data_reply(reply: &[u8], kind: u8, maximum: usize) -> Result<Vec<u8>, i32> {
        Self::success(reply)?;
        if reply.len() < 5 || reply[0] != kind {
            return Err(71);
        }
        let size = WireBytes::u32(reply, 1)? as usize;
        if size > maximum || reply.len() != 5 + size {
            return Err(71);
        }
        Ok(reply[5..].to_vec())
    }

    pub fn stat_reply(reply: &[u8]) -> Result<TreeStat, i32> {
        Self::success(reply)?;
        if reply.len() != 29 || reply[0] != STAT {
            return Err(71);
        }
        Ok(TreeStat {
            size: WireBytes::u64(reply, 1)?,
            mode: WireBytes::u32(reply, 9)?,
            device: WireBytes::u64(reply, 13)?,
            inode: WireBytes::u64(reply, 21)?,
        })
    }

    pub fn close_reply(reply: &[u8]) -> Result<(), i32> {
        Self::success(reply)?;
        (reply == [CLOSE]).then_some(()).ok_or(71)
    }

    pub(super) fn error(errno: i32) -> Vec<u8> {
        let mut value = vec![ERROR];
        value.extend(errno.to_le_bytes());
        value.extend([0, 0]);
        value
    }

    fn success(reply: &[u8]) -> Result<(), i32> {
        if reply.len() == 7 && reply[0] == ERROR {
            let errno = i32::from_le_bytes(reply[1..5].try_into().unwrap());
            if reply[5..] == [0, 0] && (1..=4095).contains(&errno) {
                return Err(errno);
            }
        }
        Ok(())
    }

    fn count(reply: &[u8], kind: u8, maximum: usize) -> Result<usize, i32> {
        Self::success(reply)?;
        if reply.len() != 5 || reply[0] != kind {
            return Err(71);
        }
        let count = WireBytes::u32(reply, 1)? as usize;
        (count <= maximum).then_some(count).ok_or(71)
    }

    fn handle_request(kind: u8, handle: u64) -> Vec<u8> {
        let mut value = vec![kind];
        WireBytes::put_u64(&mut value, handle);
        value
    }

    fn byte_request(kind: u8, handle: u64, size: usize) -> Result<Vec<u8>, i32> {
        let size = u32::try_from(size).map_err(|_| 90)?;
        let mut value = Self::handle_request(kind, handle);
        WireBytes::put_u32(&mut value, size);
        Ok(value)
    }
}

pub(super) struct WireBytes;

impl WireBytes {
    pub(super) fn put_u32(output: &mut Vec<u8>, value: u32) {
        output.extend(value.to_le_bytes());
    }

    pub(super) fn put_u64(output: &mut Vec<u8>, value: u64) {
        output.extend(value.to_le_bytes());
    }

    pub(super) fn u32(input: &[u8], offset: usize) -> Result<u32, i32> {
        input
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or(71)
    }

    pub(super) fn u64(input: &[u8], offset: usize) -> Result<u64, i32> {
        input
            .get(offset..offset + 8)
            .and_then(|value| value.try_into().ok())
            .map(u64::from_le_bytes)
            .ok_or(71)
    }
}
