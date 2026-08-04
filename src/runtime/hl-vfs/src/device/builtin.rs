use super::registry::Registration;
use crate::device::{Error, Id, NodeKind, Scope};
use crate::{GuestPathBytes, Permissions, ProjectedObjectId};

pub struct BuiltinDevice {
    pub path: &'static str,
    pub device: Id,
    pub kind: NodeKind,
    pub permissions: Permissions,
}

pub const BUILTIN_DEVICES: [BuiltinDevice; 7] = [
    BuiltinDevice {
        path: "/dev/null",
        device: Id::new(1, 3),
        kind: NodeKind::Null,
        permissions: Permissions::from_bits(0o666),
    },
    BuiltinDevice {
        path: "/dev/zero",
        device: Id::new(1, 5),
        kind: NodeKind::Zero,
        permissions: Permissions::from_bits(0o666),
    },
    BuiltinDevice {
        path: "/dev/full",
        device: Id::new(1, 7),
        kind: NodeKind::Full,
        permissions: Permissions::from_bits(0o666),
    },
    BuiltinDevice {
        path: "/dev/random",
        device: Id::new(1, 8),
        kind: NodeKind::Random,
        permissions: Permissions::from_bits(0o666),
    },
    BuiltinDevice {
        path: "/dev/urandom",
        device: Id::new(1, 9),
        kind: NodeKind::Urandom,
        permissions: Permissions::from_bits(0o666),
    },
    BuiltinDevice {
        path: "/dev/tty",
        device: Id::new(5, 0),
        kind: NodeKind::Terminal,
        permissions: Permissions::from_bits(0o666),
    },
    BuiltinDevice {
        path: "/dev/console",
        device: Id::new(5, 1),
        kind: NodeKind::Terminal,
        permissions: Permissions::from_bits(0o600),
    },
];

impl BuiltinDevice {
    pub fn registration(&self, object: ProjectedObjectId) -> Result<Registration, Error> {
        Ok(Registration {
            path: GuestPathBytes::new(self.path.as_bytes()).map_err(|_| Error::InvalidPath)?,
            scope: Scope::Root,
            device: self.device,
            kind: self.kind,
            permissions: self.permissions,
            user: 0,
            group: 0,
            object,
        })
    }
}
