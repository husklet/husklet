use std::fmt::Debug;

use hl_linux::Errno;
use hl_vfs::GuestPathBytes;

pub trait PreparedUnixSocketPathBind: Debug + Send {
    fn commit(self: Box<Self>);
    fn rollback(self: Box<Self>);
}

pub trait PreparedUnixSocketPathUnlink: Debug + Send {
    fn committed(self: Box<Self>);
}

pub trait UnixSocketPathPort: Send + Sync {
    fn prepare_bind(&self, pathname: &GuestPathBytes) -> Result<Box<dyn PreparedUnixSocketPathBind>, Errno>;

    fn prepare_unlink(&self, pathname: &GuestPathBytes) -> Option<Box<dyn PreparedUnixSocketPathUnlink>>;
}
