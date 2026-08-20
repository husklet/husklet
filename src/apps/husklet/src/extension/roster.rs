//! Every extension one workspace has, and the actions a person can take on them.
//!
//! [`Installation`] owns the lifecycle policy and [`Records`] owns the durable
//! half; neither knows about the other. This is the join: it loads what was
//! written into the policy at open, puts every action through the policy, and
//! writes back whatever the policy produced. Nothing here draws, so the whole
//! of "disable this extension" is exercised on a temporary directory with no
//! display and no container daemon.

use hl_extension::{ExtensionName, Grant, Installation, Manifest, Objection, Record, Stage};
use hl_ws::storage::{Directory, Storage};

use super::state::{Fault, Records};
use crate::config::WorkspaceConfig;

/// Why an action on the roster was refused.
#[derive(Debug)]
pub enum Refusal {
    /// The records could not be read or written.
    Record(Fault),
    /// The lifecycle policy refused the action.
    Policy(Objection),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Record(fault) => write!(formatter, "{fault}"),
            Self::Policy(objection) => write!(formatter, "{objection}"),
        }
    }
}

impl std::error::Error for Refusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Record(fault) => Some(fault),
            Self::Policy(objection) => Some(objection),
        }
    }
}

impl From<Fault> for Refusal {
    fn from(fault: Fault) -> Self {
        Self::Record(fault)
    }
}

impl From<Objection> for Refusal {
    fn from(objection: Objection) -> Self {
        Self::Policy(objection)
    }
}

/// One extension as a page shows it.
///
/// A flattened view rather than the record itself, so a screen never has to
/// consult the policy and the record separately to say where something stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Identity, which is also the sidebar label and the storage key.
    pub name: ExtensionName,
    /// The image the grant was given for.
    pub image_digest: String,
    /// Exactly what the person agreed to.
    pub granted: Grant,
    /// Where the extension stands under the lifecycle policy.
    pub stage: Stage,
}

/// Every extension recorded for one workspace, with its policy loaded.
pub struct Roster<S> {
    records: Records<S>,
    installation: Installation,
}

impl Roster<Directory> {
    /// Opens the roster of one workspace from its own storage directory.
    ///
    /// # Errors
    /// Returns `Refusal::Record` when the storage directory cannot be opened or
    /// a stored record cannot be read.
    pub fn workspace(workspace: &WorkspaceConfig) -> Result<Self, Refusal> {
        let root = workspace.storage_dir(&crate::paths::hl_root());
        let storage = Directory::open(root).map_err(|error| Fault::Storage(Box::new(error)))?;
        Self::open(storage)
    }
}

impl<S: Storage> Roster<S> {
    /// Reads what was recorded and puts every record back under the policy.
    ///
    /// A record is re-installed rather than trusted as read, so the stage a
    /// screen shows and the stage the host enforces come from one place.
    ///
    /// # Errors
    /// Returns `Refusal::Record` when the records cannot be read and
    /// `Refusal::Policy` when a stored record is refused by the policy.
    pub fn open(storage: S) -> Result<Self, Refusal> {
        let records = Records::open(storage)?;
        let mut installation = Installation::new();
        for record in records.all()? {
            enrol(&mut installation, &record)?;
        }
        Ok(Self { records, installation })
    }

    /// Every extension, ordered by name.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.installation
            .records()
            .map(|record| Entry {
                name: record.name.clone(),
                image_digest: record.image_digest.clone(),
                granted: record.granted.clone(),
                stage: self.installation.stage(&record.name),
            })
            .collect()
    }

    /// Where one extension stands.
    #[must_use]
    pub fn stage(&self, name: &ExtensionName) -> Stage {
        self.installation.stage(name)
    }

    /// Records a first install of `manifest`, granting no more than `consented`.
    ///
    /// The consent is taken as an argument rather than read from the manifest
    /// so that there is no path from an image's request to a recorded grant
    /// that does not pass through an answer a person gave.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when the name is already installed or the
    /// digest is empty, and `Refusal::Record` when the record cannot be written.
    pub fn register(&mut self, manifest: &Manifest, digest: &str, consented: &Grant, at: i64) -> Result<(), Refusal> {
        let record = self.installation.install(manifest, digest, consented, at)?.clone();
        self.records.save(&record)?;
        Ok(())
    }

    /// Marks an extension as one whose sidecar should run.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn enable(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let record = self.installation.enable(name)?.clone();
        self.records.save(&record)?;
        Ok(())
    }

    /// Marks an extension as one whose sidecar should stay down. The grant
    /// survives, so enabling it again asks nobody anything.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn disable(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let record = self.installation.disable(name)?.clone();
        self.records.save(&record)?;
        Ok(())
    }

    /// Clears a fault and puts the extension back on duty.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn retry(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let record = self.installation.retry(name)?.clone();
        self.records.save(&record)?;
        Ok(())
    }

    /// Forgets an extension entirely, grant included.
    ///
    /// Removing something that is not there succeeds, because the caller wanted
    /// it gone and it is.
    ///
    /// # Errors
    /// Returns `Refusal::Record` when the record cannot be removed.
    pub fn remove(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        self.installation.uninstall(name);
        self.records.forget(name)?;
        Ok(())
    }
}

impl<S> std::fmt::Debug for Roster<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Roster")
            .field("installed", &self.installation.len())
            .finish_non_exhaustive()
    }
}

/// The manifest a record stands for.
///
/// A record is what a person wrote down; a manifest is what an image declares.
/// Until the two are stored together the manifest is rebuilt from the record
/// alone, which is the conservative direction: it declares exactly what was
/// consented to and nothing an image might have started asking for since.
#[must_use]
pub fn described(record: &Record) -> Manifest {
    Manifest {
        name: record.name.clone(),
        display_name: record.name.to_string(),
        version: String::new(),
        protocol: hl_extension::PROTOCOL,
        capabilities: record.granted.clone(),
        entrypoint: None,
        activation: hl_extension::Activation::default(),
        interface: None,
        resources: hl_extension::Resources::default(),
        filesystem_roots: Vec::new(),
    }
}

/// Puts one stored record under the policy, in the state it was stored in.
fn enrol(installation: &mut Installation, record: &Record) -> Result<(), Objection> {
    installation.install(
        &described(record),
        &record.image_digest,
        &record.granted,
        record.installed_at,
    )?;
    if record.enabled {
        installation.enable(&record.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Refusal, Roster};
    use hl_extension::{Capability, ExtensionName, Grant, Manifest, Stage};
    use hl_ws::storage::Directory;

    fn manifest(name: &str, capabilities: &[Capability]) -> Manifest {
        Manifest {
            name: ExtensionName::new(name).expect("name"),
            display_name: name.to_owned(),
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

    fn opened(root: &std::path::Path) -> Roster<Directory> {
        Roster::open(Directory::open(root).expect("storage")).expect("roster")
    }

    #[test]
    fn a_registered_extension_is_listed_after_a_reopen() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::ContainerRead, Capability::Interface]);
        let mut roster = opened(temporary.path());

        roster
            .register(&asked, "sha256:aaaa", &Grant::new([Capability::Interface]), 7)
            .expect("registered");

        let reopened = opened(temporary.path());
        let entries = reopened.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, asked.name);
        assert!(entries[0].granted.holds(Capability::Interface));
        assert!(
            !entries[0].granted.holds(Capability::ContainerRead),
            "only what was consented to is recorded"
        );
        assert_eq!(entries[0].stage, Stage::Standby, "an install starts off duty");
    }

    #[test]
    fn enabling_and_disabling_survive_a_reopen() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");

        roster.enable(&asked.name).expect("enabled");
        assert_eq!(opened(temporary.path()).stage(&asked.name), Stage::Duty);

        roster.disable(&asked.name).expect("disabled");
        assert_eq!(opened(temporary.path()).stage(&asked.name), Stage::Standby);
    }

    #[test]
    fn a_removed_extension_leaves_no_grant_behind() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");

        roster.remove(&asked.name).expect("removed");

        assert!(roster.entries().is_empty());
        assert_eq!(opened(temporary.path()).stage(&asked.name), Stage::Vacancy);
    }

    #[test]
    fn a_second_registration_of_one_name_is_refused_rather_than_overwriting() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");

        let refused = roster
            .register(&asked, "sha256:bbbb", &asked.capabilities, 8)
            .expect_err("a second install");

        assert!(matches!(refused, Refusal::Policy(_)));
        assert_eq!(roster.entries()[0].image_digest, "sha256:aaaa");
    }

    #[test]
    fn two_extensions_are_listed_by_name() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut roster = opened(temporary.path());
        for name in ["zulu", "alpha"] {
            let asked = manifest(name, &[Capability::Interface]);
            roster
                .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
                .expect("registered");
        }

        let listed: Vec<String> = roster.entries().iter().map(|entry| entry.name.to_string()).collect();

        assert_eq!(listed, ["alpha", "zulu"], "the listing is ordered by name");
    }
}
