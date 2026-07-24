use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, num::NonZeroU16, path::PathBuf};

/// Durable root filesystem ownership used by a container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Rootfs {
    /// OCI snapshot protected by a durable `hl-images` lease.
    Image(hl_images::rootfs::Reference),
    /// Advanced unmanaged directory. The caller owns its lifetime and integrity.
    Directory(PathBuf),
}

/// Linux guest instruction set.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Guest {
    #[default]
    Aarch64,
    X86_64,
}

/// Non-empty terminal dimensions measured in character cells.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Size {
    rows: NonZeroU16,
    columns: NonZeroU16,
}

impl Size {
    /// Creates terminal dimensions, rejecting zero rows or columns.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSpec`] when either dimension is zero.
    pub fn new(rows: u16, columns: u16) -> Result<Self> {
        let rows = NonZeroU16::new(rows)
            .ok_or_else(|| Error::InvalidSpec("terminal rows must be nonzero".into()))?;
        let columns = NonZeroU16::new(columns)
            .ok_or_else(|| Error::InvalidSpec("terminal columns must be nonzero".into()))?;
        Ok(Self { rows, columns })
    }

    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows.get()
    }

    #[must_use]
    pub const fn columns(self) -> u16 {
        self.columns.get()
    }
}

impl Default for Size {
    fn default() -> Self {
        Self {
            rows: NonZeroU16::new(24).expect("24 is nonzero"),
            columns: NonZeroU16::new(80).expect("80 is nonzero"),
        }
    }
}

/// Process console policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Console {
    /// Open a pipe that live sessions may write and explicitly close.
    pub stdin: bool,
    /// Allocate a terminal at the requested initial dimensions.
    pub terminal: Option<Size>,
}

impl Console {
    #[must_use]
    pub const fn terminal(mut self, size: Size) -> Self {
        self.terminal = Some(size);
        self
    }
}

/// Initial process executed in a container.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Process {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "Process::default_working_dir")]
    pub working_dir: PathBuf,
    pub uid: Option<i32>,
    pub gid: Option<i32>,
    #[serde(default)]
    pub console: Console,
}

impl Process {
    fn default_working_dir() -> PathBuf {
        PathBuf::from("/")
    }

    /// Resolve a Linux numeric or named user/group against a root filesystem.
    ///
    /// # Errors
    /// Returns an error when account databases cannot be read or the requested identity is absent or invalid.
    pub fn resolve_user(value: &str, rootfs: &std::path::Path) -> Result<(i32, i32)> {
        let (user, explicit_group) = value
            .split_once(':')
            .map_or((value, None), |(user, group)| (user, Some(group)));
        let (uid, default_gid) = if let Ok(uid) = user.parse::<i32>() {
            (uid, uid)
        } else {
            let passwd = std::fs::read_to_string(rootfs.join("etc/passwd"))?;
            let matches = passwd
                .lines()
                .filter_map(|line| {
                    let fields: Vec<_> = line.split(':').collect();
                    (fields.len() >= 4 && fields[0] == user)
                        .then(|| Some((fields[2].parse().ok()?, fields[3].parse().ok()?)))
                        .flatten()
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [identity] => *identity,
                [] => {
                    return Err(Error::InvalidSpec(format!(
                        "user {user:?} is not present in /etc/passwd"
                    )));
                }
                _ => {
                    return Err(Error::InvalidSpec(format!(
                        "user {user:?} is ambiguous in /etc/passwd"
                    )));
                }
            }
        };
        let gid = match explicit_group {
            None => default_gid,
            Some(group) => {
                if let Ok(gid) = group.parse::<i32>() {
                    gid
                } else {
                    let groups = std::fs::read_to_string(rootfs.join("etc/group"))?;
                    let matches = groups
                        .lines()
                        .filter_map(|line| {
                            let fields: Vec<_> = line.split(':').collect();
                            (fields.len() >= 3 && fields[0] == group)
                                .then(|| fields[2].parse().ok())
                                .flatten()
                        })
                        .collect::<Vec<_>>();
                    match matches.as_slice() {
                        [gid] => *gid,
                        [] => {
                            return Err(Error::InvalidSpec(format!(
                                "group {group:?} is not present in /etc/group"
                            )));
                        }
                        _ => {
                            return Err(Error::InvalidSpec(format!(
                                "group {group:?} is ambiguous in /etc/group"
                            )));
                        }
                    }
                }
            }
        };
        if uid < 0 || gid < 0 {
            return Err(Error::InvalidSpec(
                "uid and gid must be non-negative".into(),
            ));
        }
        Ok((uid, gid))
    }
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_dir: Self::default_working_dir(),
            uid: None,
            gid: None,
            console: Console::default(),
        }
    }

    #[must_use]
    pub fn args(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args.extend(values.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn working_dir(mut self, value: impl Into<PathBuf>) -> Self {
        self.working_dir = value.into();
        self
    }

    #[must_use]
    pub const fn user(mut self, uid: i32, gid: i32) -> Self {
        self.uid = Some(uid);
        self.gid = Some(gid);
        self
    }

    #[must_use]
    pub const fn console(mut self, value: Console) -> Self {
        self.console = value;
        self
    }
}

impl Guest {
    /// Selects the execution backend for an OCI image platform.
    ///
    /// # Errors
    /// Returns an error when the engine has no backend for the platform.
    pub fn for_platform(platform: &hl_images::Platform) -> Result<Self> {
        match (platform.os.as_str(), platform.architecture.as_str()) {
            ("linux", "arm64") => Ok(Self::Aarch64),
            ("linux", "amd64") => Ok(Self::X86_64),
            _ => Err(Error::InvalidSpec(format!(
                "engine does not support guest platform {}/{}",
                platform.os, platform.architecture
            ))),
        }
    }
}

#[path = "process_spec.rs"]
mod spec;
pub use spec::ContainerSpec;
