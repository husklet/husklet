use std::collections::BTreeMap;

use crate::{Error, History, Platform, Reference, Result, RuntimeConfig, RuntimeOverrides};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recipe {
    pub stages: Vec<Stage>,
    pub selected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stage {
    pub name: Option<String>,
    pub base: Base,
    pub platform: Option<Platform>,
    pub runtime: RuntimeConfig,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub working_directory: Option<String>,
    pub user: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub history: Vec<History>,
    pub onbuild: Vec<String>,
    pub exposed_ports: std::collections::BTreeSet<String>,
    pub volumes: std::collections::BTreeSet<String>,
    pub healthcheck: Option<serde_json::Value>,
    pub stop_signal: Option<String>,
    pub steps: Vec<Step>,
}

impl Stage {
    #[must_use]
    pub fn overrides(&self) -> RuntimeOverrides {
        RuntimeOverrides {
            command: self.command.clone(),
            entrypoint: self.entrypoint.clone(),
            environment: self.runtime.environment.clone(),
            working_directory: self.working_directory.clone(),
            user: self.user.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Base {
    Image(Reference),
    Stage(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    Run {
        command: String,
        environment: BTreeMap<String, String>,
        directory: Option<String>,
        shell: Vec<String>,
        user: Option<String>,
        mounts: Vec<RunMount>,
    },
    Copy {
        sources: Vec<Source>,
        target: String,
        directory: Option<String>,
        from: Option<CopySource>,
        unpack: bool,
        mode: Option<u32>,
        ownership: Option<OwnershipSpec>,
        excludes: Vec<String>,
        parents: bool,
        checksum: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CopySource {
    Stage(usize),
    Image(Reference),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Local(String),
    Remote(String),
    Git(String),
}

impl Source {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Local(value) | Self::Remote(value) | Self::Git(value) => value,
        }
    }

    #[must_use]
    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }
}

impl From<String> for Source {
    fn from(value: String) -> Self {
        if value.starts_with("git://")
            || value.starts_with("ssh://")
            || value.starts_with("git@")
            || ((value.starts_with("http://") || value.starts_with("https://"))
                && value.split(['?', '#']).next().is_some_and(|path| {
                    std::path::Path::new(path)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("git"))
                }))
        {
            Self::Git(value)
        } else if value.starts_with("http://") || value.starts_with("https://") {
            Self::Remote(value)
        } else {
            Self::Local(value)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunMount {
    Cache {
        id: Option<String>,
        target: String,
        sharing: CacheSharing,
    },
    Bind {
        from: Option<usize>,
        source: String,
        target: String,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CacheSharing {
    #[default]
    Shared,
    Locked,
    Private,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnershipSpec {
    pub user: Account,
    pub group: Option<Account>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Account {
    Id(u32),
    Name(String),
}

impl std::fmt::Display for Account {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(value) => value.fmt(formatter),
            Self::Name(value) => formatter.write_str(value),
        }
    }
}

impl std::str::FromStr for OwnershipSpec {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let (user, group) = value
            .split_once(':')
            .map_or((value, None), |(user, group)| (user, Some(group)));
        if user.is_empty() || group.is_some_and(str::is_empty) || value.matches(':').count() > 1 {
            return Err(Error::MalformedOci(
                "COPY/ADD --chown requires uid/name[:gid/group]".into(),
            ));
        }

        Ok(Self {
            user: user.parse()?,
            group: group.map(str::parse).transpose()?,
        })
    }
}

impl std::str::FromStr for Account {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.bytes().all(|byte| byte.is_ascii_digit()) {
            return value
                .parse()
                .map(Self::Id)
                .map_err(|_| Error::MalformedOci("COPY/ADD --chown account ID is out of range".into()));
        }

        if value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Ok(Self::Name(value.into()));
        }

        Err(Error::MalformedOci(
            "COPY/ADD --chown contains an invalid account".into(),
        ))
    }
}
