use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, num::NonZeroU16, path::PathBuf};

/// Linux process environment, either OCI text or exact ordered byte records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Environment {
    Text(BTreeMap<String, String>),
    Exact(Vec<EnvironmentRecord>),
}

/// One exact Linux environment record without its `=` separator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRecord {
    name: Vec<u8>,
    value: Vec<u8>,
}

impl EnvironmentRecord {
    /// Creates one exact environment record.
    ///
    /// Full engine-limit validation is performed when the containing process
    /// specification is admitted.
    #[must_use]
    pub fn new(name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::Text(BTreeMap::new())
    }
}

impl Environment {
    fn insert_text(&mut self, name: String, value: String) {
        match self {
            Self::Text(values) => {
                values.insert(name, value);
            }
            Self::Exact(values) => {
                let name = name.into_bytes();
                let value = value.into_bytes();
                Self::replace_exact(values, &name, &value);
            }
        }
    }

    fn push_exact(&mut self, name: Vec<u8>, value: Vec<u8>) {
        self.promote_exact();
        let Self::Exact(values) = self else { unreachable!() };
        values.push(EnvironmentRecord { name, value });
    }

    fn promote_exact(&mut self) {
        if let Self::Text(values) = self {
            let exact = std::mem::take(values)
                .into_iter()
                .map(|(name, value)| EnvironmentRecord {
                    name: name.into_bytes(),
                    value: value.into_bytes(),
                })
                .collect();
            *self = Self::Exact(exact);
        }
    }

    fn replace_exact(records: &mut Vec<EnvironmentRecord>, name: &[u8], value: &[u8]) {
        let Some(index) = records.iter().position(|record| record.name == name) else {
            records.push(EnvironmentRecord {
                name: name.to_vec(),
                value: value.to_vec(),
            });
            return;
        };
        // The first occurrence takes the new value and every later one goes, so the result holds
        // the name exactly once, in the position it already had.
        records[index].value = value.to_vec();
        let mut candidate = index + 1;
        while candidate < records.len() {
            if records[candidate].name == name {
                records.remove(candidate);
                continue;
            }
            candidate += 1;
        }
    }

    pub(crate) fn records(&self) -> Vec<(&[u8], &[u8])> {
        match self {
            Self::Text(values) => values
                .iter()
                .map(|(name, value)| (name.as_bytes(), value.as_bytes()))
                .collect(),
            Self::Exact(values) => values
                .iter()
                .map(|record| (record.name.as_slice(), record.value.as_slice()))
                .collect(),
        }
    }

    pub(crate) fn text(&self) -> Result<BTreeMap<String, String>> {
        let Self::Text(values) = self else {
            return Err(Error::InvalidSpec(
                "exact ordered process environments cannot be represented in OCI image metadata".into(),
            ));
        };
        Ok(values.clone())
    }

    pub fn get_text(&self, name: &str) -> Option<&str> {
        match self {
            Self::Text(values) => values.get(name).map(String::as_str),
            Self::Exact(values) => values
                .iter()
                .find(|record| record.name == name.as_bytes())
                .and_then(|record| std::str::from_utf8(&record.value).ok()),
        }
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.records()
            .iter()
            .any(|(candidate, _)| *candidate == name.as_bytes())
    }

    pub(crate) fn overlay(&mut self, values: &Self) {
        if matches!(self, Self::Exact(_)) || matches!(values, Self::Exact(_)) {
            self.promote_exact();
        }
        match self {
            Self::Text(current) => {
                let Self::Text(values) = values else { unreachable!() };
                current.extend(values.clone());
            }
            Self::Exact(current) => {
                let additions = values
                    .records()
                    .into_iter()
                    .map(|(name, value)| EnvironmentRecord::new(name, value))
                    .collect::<Vec<_>>();
                current.retain(|record| !additions.iter().any(|addition| addition.name == record.name));
                current.extend(additions);
            }
        }
    }
}

impl Extend<(String, String)> for Environment {
    fn extend<T: IntoIterator<Item = (String, String)>>(&mut self, iter: T) {
        for (name, value) in iter {
            self.insert_text(name, value);
        }
    }
}

#[cfg(test)]
mod environment_tests {
    use super::{Environment, EnvironmentRecord};
    use std::collections::BTreeMap;

    #[test]
    fn legacy_text_map_and_exact_records_round_trip_without_losing_identity() {
        let legacy: Environment = serde_json::from_str(r#"{"B":"two","A":"one"}"#).unwrap();
        assert_eq!(
            legacy.records(),
            vec![
                (b"A".as_slice(), b"one".as_slice()),
                (b"B".as_slice(), b"two".as_slice())
            ]
        );
        assert_eq!(serde_json::to_string(&legacy).unwrap(), r#"{"A":"one","B":"two"}"#);

        let exact = Environment::Exact(vec![
            EnvironmentRecord::new(b"NAME", b"first"),
            EnvironmentRecord::new(b"RAW", b"value\xff"),
            EnvironmentRecord::new(b"NAME", b"second"),
        ]);
        let encoded = serde_json::to_vec(&exact).unwrap();
        let decoded: Environment = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, exact);
        assert_eq!(decoded.records()[1], (b"RAW".as_slice(), b"value\xff".as_slice()));
    }

    #[test]
    fn oci_text_conversion_rejects_every_exact_environment() {
        let raw = Environment::Exact(vec![EnvironmentRecord::new(b"RAW", b"value\xff")]);
        assert!(raw.text().is_err());

        let representable = Environment::Exact(vec![EnvironmentRecord::new(b"NAME", b"value")]);
        assert!(representable.text().is_err());

        let text = Environment::Text(BTreeMap::from([("NAME".to_owned(), "value".to_owned())]));
        assert_eq!(
            text.text().unwrap(),
            BTreeMap::from([("NAME".to_owned(), "value".to_owned())])
        );
    }

    #[test]
    fn exact_records_preserve_order_bytes_and_duplicate_names() {
        let environment = Environment::Exact(vec![
            super::EnvironmentRecord {
                name: b"TZ".to_vec(),
                value: b"UTC\xff".to_vec(),
            },
            super::EnvironmentRecord {
                name: b"EMPTY".to_vec(),
                value: Vec::new(),
            },
            super::EnvironmentRecord {
                name: b"TZ".to_vec(),
                value: b"later".to_vec(),
            },
        ]);
        assert_eq!(
            environment.records(),
            vec![
                (b"TZ".as_slice(), b"UTC\xff".as_slice()),
                (b"EMPTY".as_slice(), b"".as_slice()),
                (b"TZ".as_slice(), b"later".as_slice()),
            ]
        );
    }

    #[test]
    fn overlay_removes_base_duplicates_and_preserves_override_duplicates() {
        let mut environment = Environment::default();
        environment.push_exact(b"FIRST".to_vec(), b"one".to_vec());
        environment.push_exact(b"REPLACE".to_vec(), b"old".to_vec());
        environment.push_exact(b"LAST".to_vec(), b"three".to_vec());
        environment.push_exact(b"REPLACE".to_vec(), b"duplicate".to_vec());
        let overlay = Environment::Exact(vec![
            EnvironmentRecord::new(b"REPLACE", b"new"),
            EnvironmentRecord::new(b"REPLACE", b"newer"),
        ]);

        environment.overlay(&overlay);

        assert_eq!(
            environment.records(),
            vec![
                (b"FIRST".as_slice(), b"one".as_slice()),
                (b"LAST".as_slice(), b"three".as_slice()),
                (b"REPLACE".as_slice(), b"new".as_slice()),
                (b"REPLACE".as_slice(), b"newer".as_slice()),
            ]
        );
    }

    #[test]
    fn exact_overlay_keeps_base_then_overlay_declaration_order() {
        let mut environment = Environment::default();
        environment.insert_text("B".to_owned(), "base-b".to_owned());
        environment.insert_text("Z".to_owned(), "base-z".to_owned());
        let overlay = Environment::Exact(vec![
            super::EnvironmentRecord::new(b"A", b"text"),
            super::EnvironmentRecord::new(b"RAW", b"value\xff"),
            super::EnvironmentRecord::new(b"C", b"last"),
        ]);

        environment.overlay(&overlay);

        assert_eq!(
            environment.records(),
            vec![
                (b"B".as_slice(), b"base-b".as_slice()),
                (b"Z".as_slice(), b"base-z".as_slice()),
                (b"A".as_slice(), b"text".as_slice()),
                (b"RAW".as_slice(), b"value\xff".as_slice()),
                (b"C".as_slice(), b"last".as_slice()),
            ]
        );
    }

    #[test]
    fn text_insert_collapses_duplicate_exact_names() {
        let mut environment = Environment::Exact(vec![
            super::EnvironmentRecord::new(b"KEEP", b"first"),
            super::EnvironmentRecord::new(b"NAME", b"old"),
            super::EnvironmentRecord::new(b"NAME", b"duplicate"),
            super::EnvironmentRecord::new(b"TAIL", b"last"),
        ]);

        environment.insert_text("NAME".to_owned(), "new".to_owned());

        assert_eq!(
            environment.records(),
            vec![
                (b"KEEP".as_slice(), b"first".as_slice()),
                (b"NAME".as_slice(), b"new".as_slice()),
                (b"TAIL".as_slice(), b"last".as_slice()),
            ]
        );
    }
}

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
        let rows = NonZeroU16::new(rows).ok_or_else(|| Error::InvalidSpec("terminal rows must be nonzero".into()))?;
        let columns =
            NonZeroU16::new(columns).ok_or_else(|| Error::InvalidSpec("terminal columns must be nonzero".into()))?;
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
    pub args: Vec<String>,
    pub env: Environment,
    pub working_dir: PathBuf,
    pub uid: Option<i32>,
    pub gid: Option<i32>,
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
            // A missing account database is a bad request, not a missing resource: the daemon maps
            // a NotFound io error to HTTP 404, which misreports a scratch image asked for a name.
            let passwd = std::fs::read_to_string(rootfs.join("etc/passwd")).map_err(|error| {
                Error::InvalidSpec(format!("user {user:?} cannot be resolved: /etc/passwd {error}"))
            })?;
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
                    return Err(Error::InvalidSpec(format!("user {user:?} is ambiguous in /etc/passwd")));
                }
            }
        };
        let gid = match explicit_group {
            None => default_gid,
            Some(group) => {
                if let Ok(gid) = group.parse::<i32>() {
                    gid
                } else {
                    let groups = std::fs::read_to_string(rootfs.join("etc/group")).map_err(|error| {
                        Error::InvalidSpec(format!("group {group:?} cannot be resolved: /etc/group {error}"))
                    })?;
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
            return Err(Error::InvalidSpec("uid and gid must be non-negative".into()));
        }
        Ok((uid, gid))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.program.is_empty() {
            return Err(Error::InvalidSpec("process program must not be empty".into()));
        }
        if !self.working_dir.is_absolute() {
            return Err(Error::InvalidSpec("working directory must be absolute".into()));
        }
        let environment = self.env.records();
        if environment
            .iter()
            .any(|(name, _)| name.is_empty() || name.contains(&b'='))
        {
            return Err(Error::InvalidSpec(
                "environment names must be non-empty and exclude '='".into(),
            ));
        }

        for value in std::iter::once(self.program.as_bytes()).chain(self.args.iter().map(String::as_bytes)) {
            if value.contains(&0) {
                return Err(Error::InvalidSpec("process strings must not contain NUL".into()));
            }
        }
        for (name, value) in environment {
            if name.contains(&0) || value.contains(&0) {
                return Err(Error::InvalidSpec("process strings must not contain NUL".into()));
            }
        }
        Ok(())
    }
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Environment::default(),
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
        self.env.insert_text(name.into(), value.into());
        self
    }

    /// Appends one exact ordered Linux environment record.
    #[must_use]
    pub fn env_bytes(mut self, name: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) -> Self {
        self.env.push_exact(name.into(), value.into());
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
pub use spec::{ContainerSpec, Resolver};

/// Engine execution selection carried with a container launch.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum Execution {
    #[default]
    Auto,
    Interpreted,
    Translit,
    Native {
        diagnostics: bool,
    },
}

impl Execution {
    /// Selects the native execution backend, optionally recording native-boundary diagnostics.
    #[must_use]
    pub const fn native(diagnostics: bool) -> Self {
        Self::Native { diagnostics }
    }

    #[must_use]
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native { .. })
    }

    #[must_use]
    pub const fn diagnostics(self) -> bool {
        match self {
            Self::Auto | Self::Interpreted | Self::Translit => false,
            Self::Native { diagnostics } => diagnostics,
        }
    }

    /// Whether this policy selects same-ISA translation for an x86-64 guest on a supported host.
    #[must_use]
    pub const fn translit(self, x86_64_guest: bool) -> bool {
        x86_64_guest
            && cfg!(all(target_os = "linux", target_arch = "x86_64"))
            && matches!(self, Self::Auto | Self::Translit)
    }
}

#[cfg(test)]
mod execution_tests {
    use super::Execution;

    #[test]
    fn legacy_execution_encoding_is_unchanged() {
        assert_eq!(
            serde_json::to_string(&Execution::default()).unwrap(),
            r#"{"backend":"auto"}"#
        );
        assert_eq!(
            serde_json::to_string(&Execution::native(true)).unwrap(),
            r#"{"backend":"native","diagnostics":true}"#
        );
        assert_eq!(
            serde_json::from_str::<Execution>(r#"{"backend":"native","diagnostics":false}"#).unwrap(),
            Execution::native(false)
        );
    }

    #[test]
    fn retired_engine_selectors_are_rejected() {
        assert!(serde_json::from_str::<Execution>(r#"{"backend":"retained_c"}"#).is_err());
        assert!(serde_json::from_str::<Execution>(r#"{"backend":"retained_c_diagnostics"}"#).is_err());
        assert!(serde_json::from_str::<Execution>(r#"{"backend":"rust_interpreted"}"#).is_err());
        assert!(serde_json::from_str::<Execution>(r#"{"backend":"rust_native","diagnostics":true}"#).is_err());
    }
}
