//! Where a workspace keeps what it recorded about its extensions.
//!
//! [`Installation`](hl_extension::Installation) owns the lifecycle policy and
//! holds it in memory; this module is only its durable half. A [`Record`] is the
//! written form of a person's consent, so it has to outlive the process that
//! took it: without this, every restart would either ask again or — far worse —
//! start from whatever the current manifest happens to request.
//!
//! Nothing here interprets a record. A grant that was narrowed on disk comes
//! back narrow, because the only thing this module does is serialize and
//! deserialize.

use hl_extension::{ExtensionName, Record};
use hl_ws::storage::{Key, Storage};

/// Storage prefix every extension record lives below.
pub const PREFIX: &str = "state/extensions";

/// Why a record could not be read or written.
#[derive(Debug)]
pub enum Fault {
    /// The storage layer refused the read, write, or removal.
    Storage(Box<dyn std::error::Error + Send + Sync>),
    /// A stored record could not be encoded or parsed. Carries the key so the
    /// unreadable record can be named rather than reported as a bare failure.
    Format {
        /// The key whose bytes could not be understood.
        key: String,
        /// What the encoding refused.
        detail: String,
    },
}

impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "extension state storage failed: {error}"),
            Self::Format { key, detail } => write!(formatter, "extension record {key} is unreadable: {detail}"),
        }
    }
}

impl std::error::Error for Fault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error.as_ref()),
            Self::Format { .. } => None,
        }
    }
}

/// The extension records of one workspace, on that workspace's storage.
pub struct Records<S> {
    storage: S,
    prefix: Key,
}

impl<S: Storage> Records<S> {
    /// Opens the record area of one workspace's storage.
    ///
    /// # Errors
    /// Returns `Fault::Storage` when the prefix is not a usable storage key,
    /// which can only happen if [`PREFIX`] is changed to something invalid.
    pub fn open(storage: S) -> Result<Self, Fault> {
        let prefix = Key::parse(PREFIX).map_err(|error| Fault::Storage(Box::new(error)))?;
        Ok(Self { storage, prefix })
    }

    /// Every record this workspace has, in key order.
    ///
    /// An unreadable record fails the whole load rather than being skipped: a
    /// record that quietly disappears reads to a person as an extension that
    /// uninstalled itself, and the grant it carried would be gone with it.
    ///
    /// # Errors
    /// Returns `Fault::Storage` when the listing or a read fails, and
    /// `Fault::Format` when a stored record cannot be parsed.
    pub fn all(&self) -> Result<Vec<Record>, Fault> {
        let keys = self.storage.list(Some(&self.prefix)).map_err(fault)?;
        let mut records = Vec::with_capacity(keys.len());
        for key in keys {
            let bytes = self.storage.get(&key).map_err(fault)?;
            records.push(parse(&key, &bytes)?);
        }
        Ok(records)
    }

    /// Writes one record, replacing whatever was stored under its name.
    ///
    /// # Errors
    /// Returns `Fault::Format` when the record cannot be serialized and
    /// `Fault::Storage` when the write fails.
    pub fn save(&self, record: &Record) -> Result<(), Fault> {
        let key = self.key(&record.name)?;
        let bytes = serde_json::to_vec(record).map_err(|error| Fault::Format {
            key: key.to_string(),
            detail: error.to_string(),
        })?;
        self.storage.put(&key, &bytes).map_err(fault)
    }

    /// Forgets one record entirely.
    ///
    /// Removing an absent record succeeds, because the caller wanted it gone
    /// and it is.
    ///
    /// # Errors
    /// Returns `Fault::Storage` when the removal fails.
    pub fn forget(&self, name: &ExtensionName) -> Result<(), Fault> {
        let key = self.key(name)?;
        self.storage.remove(&key).map_err(fault)
    }

    /// The key one extension's record is stored under.
    fn key(&self, name: &ExtensionName) -> Result<Key, Fault> {
        self.prefix
            .join(name.as_str())
            .map_err(|error| Fault::Storage(Box::new(error)))
    }
}

/// Boxes a storage error so callers see one error type whatever backs storage.
fn fault<E: std::error::Error + Send + Sync + 'static>(error: E) -> Fault {
    Fault::Storage(Box::new(error))
}

/// Parses one stored record, naming the key that held it.
fn parse(key: &Key, bytes: &[u8]) -> Result<Record, Fault> {
    serde_json::from_slice(bytes).map_err(|error| Fault::Format {
        key: key.to_string(),
        detail: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{Fault, Records, PREFIX};
    use hl_extension::{Capability, ExtensionName, Grant, Installation, Manifest, Record};
    use hl_ws::storage::{Directory, Key, Storage as _};

    fn manifest(capabilities: &[Capability]) -> Manifest {
        Manifest {
            name: ExtensionName::new("sample").expect("name"),
            display_name: "Sample".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: hl_extension::PROTOCOL,
            capabilities: Grant::new(capabilities.iter().copied()),
            entrypoint: None,
            activation: hl_extension::Activation::default(),
            interface: None,
            resources: hl_extension::Resources::default(),
            filesystem_roots: Vec::new(),
        }
    }

    fn records(root: &std::path::Path) -> Records<Directory> {
        Records::open(Directory::open(root).expect("storage")).expect("records")
    }

    #[test]
    fn a_record_survives_being_written_and_read_back() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let records = records(temporary.path());
        let record = Record {
            name: ExtensionName::new("sample").expect("name"),
            image_digest: "sha256:aaaa".to_owned(),
            granted: Grant::new([Capability::ContainerRead, Capability::Interface]),
            enabled: true,
            installed_at: 1_700_000_000,
        };

        records.save(&record).expect("saved");

        assert_eq!(records.all().expect("loaded"), vec![record]);
    }

    #[test]
    fn a_forgotten_record_leaves_nothing_behind() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let records = records(temporary.path());
        let name = ExtensionName::new("sample").expect("name");
        records
            .save(&Record {
                name: name.clone(),
                image_digest: "sha256:aaaa".to_owned(),
                granted: Grant::new([Capability::ContainerRead]),
                enabled: false,
                installed_at: 1,
            })
            .expect("saved");

        records.forget(&name).expect("forgotten");
        records.forget(&name).expect("forgetting twice is not a failure");

        assert!(records.all().expect("loaded").is_empty());
        assert!(!temporary.path().join(PREFIX).join("sample").exists());
    }

    #[test]
    fn a_narrow_grant_stays_narrow_across_a_restart() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest(&[Capability::ContainerRead, Capability::ContainerControl]);
        let mut installation = Installation::new();
        let recorded = installation
            .install(&asked, "sha256:aaaa", &Grant::new([Capability::ContainerRead]), 5)
            .expect("installed")
            .clone();
        records(temporary.path()).save(&recorded).expect("saved");

        // A second process, reading what the first wrote.
        let reloaded = records(temporary.path()).all().expect("loaded");

        assert_eq!(reloaded.len(), 1);
        assert!(reloaded[0].granted.holds(Capability::ContainerRead));
        assert!(
            !reloaded[0].granted.holds(Capability::ContainerControl),
            "a restart must not hand over what the manifest asked for"
        );
        assert!(!reloaded[0].granted.covers(&asked.capabilities));
    }

    #[test]
    fn an_unreadable_record_is_named_rather_than_skipped() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let storage = Directory::open(temporary.path()).expect("storage");
        storage
            .put(&Key::parse(format!("{PREFIX}/sample")).expect("key"), b"not a record")
            .expect("written");

        let fault = records(temporary.path()).all().expect_err("unreadable");

        assert!(matches!(fault, Fault::Format { .. }));
        assert!(fault.to_string().contains("sample"));
    }
}
