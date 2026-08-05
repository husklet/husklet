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

pub const MAX_ID_RANGES: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdRange {
    pub inside: u32,
    pub outside: u32,
    pub length: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdMap {
    ranges: Vec<IdRange>,
}

impl IdMap {
    pub fn new(ranges: &[IdRange]) -> Result<Self, MapError> {
        if ranges.is_empty() || ranges.len() > MAX_ID_RANGES {
            return Err(MapError::Invalid);
        }
        for (index, range) in ranges.iter().enumerate() {
            if range.length == 0
                || range.inside.checked_add(range.length - 1).is_none()
                || range.outside.checked_add(range.length - 1).is_none()
            {
                return Err(MapError::Invalid);
            }
            if !Self::distinct(*range, &ranges[..index]) {
                return Err(MapError::Invalid);
            }
        }
        Ok(Self {
            ranges: ranges.to_vec(),
        })
    }

    #[must_use]
    pub fn ranges(&self) -> &[IdRange] {
        &self.ranges
    }

    #[must_use]
    pub fn single(&self, outside: u32) -> bool {
        self.ranges
            == [IdRange {
                inside: 0,
                outside,
                length: 1,
            }]
    }

    fn overlaps(first: u32, second: u32, first_len: u32, second_len: u32) -> bool {
        let first_end = first + first_len - 1;
        let second_end = second + second_len - 1;
        first <= second_end && second <= first_end
    }

    fn conflicts(first: IdRange, second: IdRange) -> bool {
        Self::overlaps(first.inside, second.inside, first.length, second.length)
            || Self::overlaps(first.outside, second.outside, first.length, second.length)
    }

    fn distinct(range: IdRange, previous: &[IdRange]) -> bool {
        for other in previous {
            if Self::conflicts(range, *other) {
                return false;
            }
        }
        true
    }
}

impl std::str::FromStr for IdMap {
    type Err = MapError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        if source.is_empty() || source.len() > 4096 || source.as_bytes().contains(&0) {
            return Err(MapError::Invalid);
        }
        let mut ranges = Vec::new();
        for line in source.lines() {
            let mut values = Vec::new();
            for value in line.split_ascii_whitespace() {
                values.push(value.parse::<u32>().ok().ok_or(MapError::Invalid)?);
            }
            if values.len() != 3 {
                return Err(MapError::Invalid);
            }
            ranges.push(IdRange {
                inside: values[0],
                outside: values[1],
                length: values[2],
            });
        }
        Self::new(&ranges)
    }
}

impl std::fmt::Display for IdMap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for range in &self.ranges {
            writeln!(formatter, "{} {} {}", range.inside, range.outside, range.length)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MapError {
    Invalid,
    Permission,
    Written,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetgroupsState {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserNamespace {
    pub id: NamespaceId,
    pub parent: Option<NamespaceId>,
    pub owner: u32,
    pub user_map: Option<IdMap>,
    pub group_map: Option<IdMap>,
    pub setgroups: SetgroupsState,
    pub user_authority: bool,
    pub group_authority: bool,
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
