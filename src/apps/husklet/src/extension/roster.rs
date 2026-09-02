//! Every extension one workspace has, and the actions a person can take on them.
//!
//! [`Installation`] owns the lifecycle policy and [`Records`] owns the durable
//! half; neither knows about the other. This is the join: it loads what was
//! written into the policy at open, puts every action through the policy, and
//! writes back whatever the policy produced. Nothing here draws, so the whole
//! of "disable this extension" is exercised on a temporary directory with no
//! display and no container daemon.

use hl_extension::{ExtensionName, Grant, Installation, Manifest, Objection, Record, Stage, Update, UpdateFailure};
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

#[derive(Debug)]
pub enum UpdateRefusal {
    Policy(Objection),
    Record(Fault),
}

impl std::fmt::Display for UpdateRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Policy(objection) => write!(formatter, "{objection}"),
            Self::Record(fault) => write!(formatter, "{fault}"),
        }
    }
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
    /// Manifest version consented to for this digest.
    pub version: String,
    /// Exactly what the person agreed to.
    pub granted: Grant,
    /// Where the extension stands under the lifecycle policy.
    pub stage: Stage,
    /// Named views this installed image offers to terminal panes.
    pub pane_providers: Vec<hl_extension::PaneProvider>,
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
    #[cfg(test)]
    pub(crate) fn enabled_record(&self, name: &ExtensionName) -> Result<Option<Record>, Refusal> {
        Ok(self
            .records
            .all()?
            .into_iter()
            .find(|record| record.name == *name && record.enabled))
    }

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
            if let Some(restarts) = records.fault(&record.name)? {
                installation.fault(&record.name, restarts)?;
            }
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
                version: record.version.clone(),
                granted: record.granted.clone(),
                stage: self.installation.stage(&record.name),
                pane_providers: record.pane_providers.clone(),
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
        let previous = self.installation.clone();
        let record = self.installation.install(manifest, digest, consented, at)?.clone();
        if let Err(fault) = self.records.save(&record) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    /// Prepares an update prompt without changing the installed record.
    pub fn prepare_update(&self, manifest: &Manifest, digest: &str) -> Result<Update, Refusal> {
        self.installation
            .prepare_update(manifest, digest)
            .map_err(Refusal::Policy)
    }

    /// Durably replaces a consented record. Saving is the replacement callback,
    /// so either both in-memory policy and durable authority advance or neither
    /// does; the old host remains mounted until the caller refreshes afterward.
    pub fn commit_update(&mut self, update: Update, consented: &Grant, at: i64) -> Result<(), UpdateRefusal> {
        let records = &self.records;
        self.installation
            .commit_update(update, consented, at, |_, next| records.save(next))
            .map(|_| ())
            .map_err(|failure| match failure {
                UpdateFailure::Refused(objection) => UpdateRefusal::Policy(objection),
                UpdateFailure::Replacement(fault) => UpdateRefusal::Record(fault),
            })
    }

    /// Marks an extension as one whose sidecar should run.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn enable(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        let record = self.installation.enable(name)?.clone();
        if let Err(fault) = self.records.save(&record) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    pub fn enable_if_digest(&mut self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        self.require_digest(name, image_digest)?;
        self.enable(name)
    }

    /// Marks an extension as one whose sidecar should stay down. The grant
    /// survives, so enabling it again asks nobody anything.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn disable(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        let record = self.installation.disable(name)?.clone();
        if let Err(fault) = self.records.save(&record) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    pub fn disable_if_digest(&mut self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        self.require_digest(name, image_digest)?;
        self.disable(name)
    }

    fn require_digest(&self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        let current = self.entries().into_iter().find(|entry| entry.name == *name);
        if current.as_ref().map(|entry| entry.image_digest.as_str()) == Some(image_digest) {
            Ok(())
        } else {
            Err(Objection::Changed(name.clone()).into())
        }
    }

    pub fn retry_if_digest(&mut self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        self.require_digest(name, image_digest)?;
        self.retry(name)
    }

    /// Clears a fault and puts the extension back on duty.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn retry(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        let record = self.installation.retry(name)?.clone();
        if let Err(fault) = self.records.save(&record).and_then(|()| self.records.clear_fault(name)) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    /// Records a crash loop observed by the live host and makes it visible to
    /// every central Settings page, including after an application restart.
    pub fn fault(&mut self, name: &ExtensionName, restarts: u32) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        self.installation.fault(name, restarts)?;
        if let Err(fault) = self.records.save_fault(name, restarts) {
            self.installation = previous;
            return Err(fault.into());
        }
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
        let previous = self.installation.clone();
        self.installation.uninstall(name);
        if let Err(fault) = self.records.forget(name) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    /// Forget only the exact image incarnation the caller inspected.
    pub fn remove_if_digest(&mut self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        let current = self.entries().into_iter().find(|entry| entry.name == *name);
        if current.as_ref().map(|entry| entry.image_digest.as_str()) != Some(image_digest) {
            return Err(Objection::Changed(name.clone()).into());
        }
        self.remove(name)
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
        version: record.version.clone(),
        protocol: hl_extension::PROTOCOL,
        capabilities: record.granted.clone(),
        entrypoint: None,
        activation: hl_extension::Activation::default(),
        interface: None,
        pane_providers: record.pane_providers.clone(),
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
            pane_providers: Vec::new(),
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
    fn a_host_fault_survives_reopen_and_retry_clears_only_the_fault() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");
        roster.enable(&asked.name).expect("enabled");

        roster.fault(&asked.name, 6).expect("fault persisted");
        assert_eq!(
            opened(temporary.path()).stage(&asked.name),
            Stage::Fault { restarts: 6 }
        );

        roster.retry(&asked.name).expect("retried");
        let reopened = opened(temporary.path());
        assert_eq!(reopened.stage(&asked.name), Stage::Duty);
        let entry = &reopened.entries()[0];
        assert_eq!(entry.image_digest, "sha256:aaaa", "retry keeps the installed image");
        assert!(entry.granted.holds(Capability::Interface), "retry keeps consent");
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
    fn removal_consent_cannot_remove_a_reinstalled_digest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster.register(&asked, "sha256:new", &asked.capabilities, 7).expect("registered");
        assert!(roster.remove_if_digest(&asked.name, "sha256:old").is_err());
        assert_eq!(roster.entries()[0].image_digest, "sha256:new");
        assert_eq!(opened(temporary.path()).entries()[0].image_digest, "sha256:new");
    }

    #[test]
    fn stale_state_change_cannot_control_a_reinstalled_digest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster.register(&asked, "sha256:new", &asked.capabilities, 7).expect("registered");
        assert!(roster.enable_if_digest(&asked.name, "sha256:old").is_err());
        assert!(roster.disable_if_digest(&asked.name, "sha256:old").is_err());
        assert_eq!(roster.stage(&asked.name), Stage::Standby);
    }

    #[test]
    fn delayed_enable_loses_to_a_concurrent_reinstallation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut initial = opened(temporary.path());
        initial.register(&asked, "sha256:old", &asked.capabilities, 7).expect("registered");
        let roster = std::sync::Arc::new(std::sync::Mutex::new(initial));
        let replaced = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_roster = roster.clone();
        let worker_barrier = replaced.clone();
        let worker_name = asked.name.clone();
        let worker_manifest = asked.clone();
        let worker = std::thread::spawn(move || {
            let mut roster = worker_roster.lock().unwrap();
            roster.remove(&worker_name).unwrap();
            roster
                .register(&worker_manifest, "sha256:new", &worker_manifest.capabilities, 8)
                .unwrap();
            worker_barrier.wait();
        });
        replaced.wait();
        assert!(roster.lock().unwrap().enable_if_digest(&asked.name, "sha256:old").is_err());
        worker.join().unwrap();
        assert_eq!(opened(temporary.path()).stage(&asked.name), Stage::Standby);
    }

    #[test]
    fn delayed_removal_loses_to_a_concurrent_reinstallation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut initial = opened(temporary.path());
        initial.register(&asked, "sha256:old", &asked.capabilities, 7).expect("registered");
        let roster = std::sync::Arc::new(std::sync::Mutex::new(initial));
        let replaced = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_roster = roster.clone();
        let worker_barrier = replaced.clone();
        let worker_name = asked.name.clone();
        let worker_manifest = asked.clone();
        let worker = std::thread::spawn(move || {
            let mut roster = worker_roster.lock().unwrap();
            roster.remove(&worker_name).unwrap();
            roster
                .register(&worker_manifest, "sha256:new", &worker_manifest.capabilities, 8)
                .unwrap();
            worker_barrier.wait();
        });
        replaced.wait();
        assert!(roster.lock().unwrap().remove_if_digest(&asked.name, "sha256:old").is_err());
        worker.join().unwrap();
        assert_eq!(opened(temporary.path()).entries()[0].image_digest, "sha256:new");
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
    fn failed_persistence_never_leaves_install_or_run_authority_in_memory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("workspace");
        std::fs::create_dir(&root).expect("workspace storage root");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(&root);

        std::fs::remove_dir(&root).expect("empty storage root");
        std::fs::write(&root, b"not a directory").expect("jam storage path");
        assert!(
            roster.register(&asked, "sha256:aaaa", &asked.capabilities, 7).is_err(),
            "the durable write is refused"
        );
        assert_eq!(
            roster.stage(&asked.name),
            Stage::Vacancy,
            "failed consent persistence grants nothing"
        );

        std::fs::remove_file(&root).expect("clear jammed path");
        std::fs::create_dir(&root).expect("restore storage root");
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("the same consent can be retried");
        assert_eq!(roster.stage(&asked.name), Stage::Standby);

        std::fs::remove_dir_all(&root).expect("remove recorded storage");
        std::fs::write(&root, b"not a directory").expect("jam storage again");
        assert!(roster.enable(&asked.name).is_err(), "enabling cannot be persisted");
        assert_eq!(
            roster.stage(&asked.name),
            Stage::Standby,
            "a failed enable cannot start a sidecar or advertise providers"
        );
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
