#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum NamespaceKind {
    Uts,
    Ipc,
    Network,
    Mount,
    User,
    Pid,
}

impl NamespaceKind {
    #[must_use]
    pub const fn clone_flag(self) -> u64 {
        match self {
            Self::Uts => 0x0400_0000,
            Self::Ipc => 0x0800_0000,
            Self::Network => 0x4000_0000,
            Self::Mount => 0x0002_0000,
            Self::User => 0x1000_0000,
            Self::Pid => 0x2000_0000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NamespaceId {
    pub kind: NamespaceKind,
    pub serial: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserNamespace {
    pub id: NamespaceId,
    pub parent: Option<NamespaceId>,
    pub owner: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NamespaceSet {
    pub uts: NamespaceId,
    pub ipc: NamespaceId,
    pub network: NamespaceId,
    pub mount: NamespaceId,
    pub user: NamespaceId,
    pub pid: NamespaceId,
}

pub const UTS_NAME_MAXIMUM: usize = 64;

/// Mutable identity owned by one UTS namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UtsIdentity {
    pub hostname: Vec<u8>,
    pub domainname: Vec<u8>,
    owner: NamespaceId,
}

impl UtsIdentity {
    pub fn new(hostname: Vec<u8>, domainname: Vec<u8>) -> Result<Self, crate::TaskError> {
        Self::owned(hostname, domainname, NamespaceSet::initial().user)
    }

    pub fn owned(hostname: Vec<u8>, domainname: Vec<u8>, owner: NamespaceId) -> Result<Self, crate::TaskError> {
        if hostname.len() > UTS_NAME_MAXIMUM || domainname.len() > UTS_NAME_MAXIMUM {
            return Err(crate::TaskError::InvalidCapacity);
        }
        if owner.kind != NamespaceKind::User || owner.serial == 0 {
            return Err(crate::TaskError::InvalidSnapshot);
        }
        Ok(Self {
            hostname,
            domainname,
            owner,
        })
    }

    #[must_use]
    pub const fn owner(&self) -> NamespaceId {
        self.owner
    }

    pub(crate) fn initial() -> Self {
        Self {
            hostname: b"jit".to_vec(),
            domainname: Vec::new(),
            owner: NamespaceSet::initial().user,
        }
    }
}

impl NamespaceSet {
    pub(crate) const fn initial() -> Self {
        Self {
            uts: NamespaceId {
                kind: NamespaceKind::Uts,
                serial: 1,
            },
            ipc: NamespaceId {
                kind: NamespaceKind::Ipc,
                serial: 1,
            },
            network: NamespaceId {
                kind: NamespaceKind::Network,
                serial: 1,
            },
            mount: NamespaceId {
                kind: NamespaceKind::Mount,
                serial: 1,
            },
            user: NamespaceId {
                kind: NamespaceKind::User,
                serial: 1,
            },
            pid: NamespaceId {
                kind: NamespaceKind::Pid,
                serial: 1,
            },
        }
    }

    #[must_use]
    pub fn get(self, kind: NamespaceKind) -> NamespaceId {
        match kind {
            NamespaceKind::Uts => self.uts,
            NamespaceKind::Ipc => self.ipc,
            NamespaceKind::Network => self.network,
            NamespaceKind::Mount => self.mount,
            NamespaceKind::User => self.user,
            NamespaceKind::Pid => self.pid,
        }
    }

    pub(crate) fn replace(&mut self, identifier: NamespaceId) {
        match identifier.kind {
            NamespaceKind::Uts => self.uts = identifier,
            NamespaceKind::Ipc => self.ipc = identifier,
            NamespaceKind::Network => self.network = identifier,
            NamespaceKind::Mount => self.mount = identifier,
            NamespaceKind::User => self.user = identifier,
            NamespaceKind::Pid => self.pid = identifier,
        }
    }

    pub(crate) fn valid(self, next: u64) -> bool {
        [
            (self.uts, NamespaceKind::Uts),
            (self.ipc, NamespaceKind::Ipc),
            (self.network, NamespaceKind::Network),
            (self.mount, NamespaceKind::Mount),
            (self.user, NamespaceKind::User),
            (self.pid, NamespaceKind::Pid),
        ]
        .into_iter()
        .all(|(identifier, kind)| identifier.kind == kind && identifier.serial != 0 && identifier.serial < next)
    }
}
