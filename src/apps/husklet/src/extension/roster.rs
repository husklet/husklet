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

fn registration_lock() -> std::sync::MutexGuard<'static, ()> {
    static REGISTRATIONS: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    REGISTRATIONS
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
    /// Human-facing identity declared by the consented image.
    pub display_name: String,
    /// How the extension asks to appear in workspace navigation.
    pub interface: Option<hl_extension::Presentation>,
    /// The image the grant was given for.
    pub image_digest: String,
    /// Manifest version consented to for this digest.
    pub version: String,
    /// Exactly what the person agreed to.
    pub granted: Grant,
    pub containers: hl_extension::ContainerGrant,
    pub images: hl_extension::ImageGrant,
    pub networks: hl_extension::NetworkGrant,
    pub volumes: hl_extension::VolumeGrant,
    pub filesystem: hl_extension::FilesystemGrant,
    pub workspace_environment: hl_extension::WorkspaceEnvironmentGrant,
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
    fn reload(&mut self) -> Result<(), Refusal> {
        let mut installation = Installation::new();
        for record in self.records.all()? {
            enrol(&mut installation, &record)?;
            if let Some(restarts) = self.records.fault(&record.name)? {
                installation.fault(&record.name, restarts)?;
            }
        }
        self.installation = installation;
        Ok(())
    }
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
                display_name: record
                    .declaration
                    .as_ref()
                    .map_or_else(|| record.name.to_string(), |manifest| manifest.display_name.clone()),
                interface: record
                    .declaration
                    .as_ref()
                    .and_then(|manifest| manifest.interface.clone()),
                image_digest: record.image_digest.clone(),
                version: record.version.clone(),
                granted: record.granted.clone(),
                containers: record.containers.clone(),
                images: record.images.clone(),
                networks: record.networks.clone(),
                volumes: record.volumes.clone(),
                filesystem: record.filesystem.clone(),
                workspace_environment: record.workspace_environment.clone(),
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

    /// Whether the complete persisted declaration and consent match a manifest.
    /// Reconstructing through [`described`] includes launch-only fields that the
    /// flattened management entry intentionally does not expose.
    #[must_use]
    pub(crate) fn matches_manifest(&self, name: &ExtensionName, manifest: &Manifest) -> bool {
        self.installation
            .record(name)
            .is_some_and(|record| described(record) == *manifest)
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
        self.register_scoped(
            manifest,
            digest,
            consented,
            &hl_extension::ContainerGrant::default(),
            at,
        )
    }

    pub fn register_scoped(
        &mut self,
        manifest: &Manifest,
        digest: &str,
        consented: &Grant,
        containers: &hl_extension::ContainerGrant,
        at: i64,
    ) -> Result<(), Refusal> {
        self.register_resource_scoped(
            manifest,
            digest,
            consented,
            containers,
            &hl_extension::ImageGrant::default(),
            &hl_extension::NetworkGrant::default(),
            &hl_extension::VolumeGrant::default(),
            &hl_extension::FilesystemGrant::default(),
            &hl_extension::WorkspaceEnvironmentGrant::default(),
            at,
        )
    }

    pub fn register_resource_scoped(
        &mut self,
        manifest: &Manifest,
        digest: &str,
        consented: &Grant,
        containers: &hl_extension::ContainerGrant,
        images: &hl_extension::ImageGrant,
        networks: &hl_extension::NetworkGrant,
        volumes: &hl_extension::VolumeGrant,
        filesystem: &hl_extension::FilesystemGrant,
        workspace_environment: &hl_extension::WorkspaceEnvironmentGrant,
        at: i64,
    ) -> Result<(), Refusal> {
        let _registration = registration_lock();
        self.reload()?;
        let previous = self.installation.clone();
        let record = self
            .installation
            .install_resource_scoped(
                manifest,
                digest,
                consented,
                containers,
                images,
                networks,
                volumes,
                filesystem,
                workspace_environment,
                at,
            )?
            .clone();
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

    /// Prepares only while the installed generation still matches the one
    /// shown with the consent prompt.
    pub fn prepare_update_if_digest(
        &self,
        manifest: &Manifest,
        digest: &str,
        installed_digest: &str,
    ) -> Result<Update, Refusal> {
        self.require_digest(&manifest.name, installed_digest)?;
        self.prepare_update(manifest, digest)
    }

    /// Durably replaces a consented record. Saving is the replacement callback,
    /// so either both in-memory policy and durable authority advance or neither
    /// does; the old host remains mounted until the caller refreshes afterward.
    pub fn commit_update(&mut self, update: Update, consented: &Grant, at: i64) -> Result<(), UpdateRefusal> {
        self.commit_update_scoped(update, consented, &hl_extension::ContainerGrant::default(), at)
    }

    pub fn commit_update_scoped(
        &mut self,
        update: Update,
        consented: &Grant,
        containers: &hl_extension::ContainerGrant,
        at: i64,
    ) -> Result<(), UpdateRefusal> {
        self.commit_update_resource_scoped(
            update,
            consented,
            containers,
            &hl_extension::ImageGrant::default(),
            &hl_extension::NetworkGrant::default(),
            &hl_extension::VolumeGrant::default(),
            &hl_extension::FilesystemGrant::default(),
            &hl_extension::WorkspaceEnvironmentGrant::default(),
            at,
        )
    }

    pub fn commit_update_resource_scoped(
        &mut self,
        update: Update,
        consented: &Grant,
        containers: &hl_extension::ContainerGrant,
        images: &hl_extension::ImageGrant,
        networks: &hl_extension::NetworkGrant,
        volumes: &hl_extension::VolumeGrant,
        filesystem: &hl_extension::FilesystemGrant,
        workspace_environment: &hl_extension::WorkspaceEnvironmentGrant,
        at: i64,
    ) -> Result<(), UpdateRefusal> {
        let records = &self.records;
        self.installation
            .commit_update_resource_scoped(
                update,
                consented,
                containers,
                images,
                networks,
                volumes,
                filesystem,
                workspace_environment,
                at,
                |_, next| records.save(next),
            )
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
        self.enable_loaded(name)
    }

    fn enable_loaded(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        let record = self.installation.enable(name)?.clone();
        if let Err(fault) = self.records.save(&record) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    pub fn enable_if_digest(&mut self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        let _transition = registration_lock();
        self.reload()?;
        self.require_digest(name, image_digest)?;
        self.enable_loaded(name)
    }

    /// Marks an extension as one whose sidecar should stay down. The grant
    /// survives, so enabling it again asks nobody anything.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn disable(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        self.disable_loaded(name)
    }

    fn disable_loaded(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        let previous_record = previous.record(name).cloned();
        let record = self.installation.disable(name)?.clone();
        if let Err(fault) = self.records.save(&record) {
            self.installation = previous;
            return Err(fault.into());
        }
        if let Err(fault) = self.records.clear_fault(name) {
            // The record and crash marker form one lifecycle decision. Restore
            // the former enabled record when clearing the latter is refused,
            // so neither memory nor a reopen observes a half-disable.
            if let Some(previous_record) = previous_record.as_ref() {
                let _ = self.records.save(previous_record);
            }
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(())
    }

    pub fn disable_if_digest(&mut self, name: &ExtensionName, image_digest: &str) -> Result<(), Refusal> {
        let _transition = registration_lock();
        self.reload()?;
        self.require_digest(name, image_digest)?;
        self.disable_loaded(name)
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
        let _transition = registration_lock();
        self.reload()?;
        self.require_digest(name, image_digest)?;
        self.retry_loaded(name)
    }

    /// Clears a fault and puts the extension back on duty.
    ///
    /// # Errors
    /// Returns `Refusal::Policy` when nothing is recorded under `name`, and
    /// `Refusal::Record` when the record cannot be written.
    pub fn retry(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
        self.retry_loaded(name)
    }

    fn retry_loaded(&mut self, name: &ExtensionName) -> Result<(), Refusal> {
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
        self.fault_loaded(name, restarts)
    }

    /// Records a crash only for the exact sidecar incarnation the host observed.
    pub fn fault_if_digest(
        &mut self,
        name: &ExtensionName,
        image_digest: &str,
        restarts: u32,
    ) -> Result<(), Refusal> {
        let _transition = registration_lock();
        self.reload()?;
        self.require_digest(name, image_digest)?;
        self.fault_loaded(name, restarts)
    }

    fn fault_loaded(&mut self, name: &ExtensionName, restarts: u32) -> Result<(), Refusal> {
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
        self.take_if_digest(name, image_digest).map(|_| ())
    }

    pub(crate) fn take_if_digest(
        &mut self,
        name: &ExtensionName,
        image_digest: &str,
    ) -> Result<Record, Refusal> {
        let _removal = registration_lock();
        self.reload()?;
        let current = self.entries().into_iter().find(|entry| entry.name == *name);
        if current.as_ref().map(|entry| entry.image_digest.as_str()) != Some(image_digest) {
            return Err(Objection::Changed(name.clone()).into());
        }
        let previous = self.installation.clone();
        let record = self
            .installation
            .uninstall(name)
            .expect("digest check proved the record exists");
        if let Err(fault) = self.records.forget(name) {
            self.installation = previous;
            return Err(fault.into());
        }
        Ok(record)
    }

    pub(crate) fn restore(&mut self, record: &Record) -> Result<(), Refusal> {
        let previous = self.installation.clone();
        enrol(&mut self.installation, record)?;
        if let Err(fault) = self.records.save(record) {
            self.installation = previous;
            return Err(fault.into());
        }
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
/// New records retain the accepted declaration so launch and presentation
/// policy survives a host restart. Legacy records are rebuilt conservatively.
/// In both cases the record's separate identity and grant overwrite the nested
/// declaration, so persistence can never turn a request into authority.
#[must_use]
pub fn described(record: &Record) -> Manifest {
    let mut manifest = record.declaration.clone().unwrap_or_else(|| Manifest {
        containers: hl_extension::ContainerGrant::default(),
        images: hl_extension::ImageGrant::default(),
        networks: hl_extension::NetworkGrant::default(),
        volumes: hl_extension::VolumeGrant::default(),
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
        filesystem: hl_extension::FilesystemGrant::default(),
        workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
    });
    // These duplicated fields are the durable consent boundary. A nested
    // declaration can describe launch and presentation, never widen authority.
    manifest.name.clone_from(&record.name);
    manifest.version.clone_from(&record.version);
    manifest.protocol = hl_extension::PROTOCOL;
    manifest.capabilities.clone_from(&record.granted);
    manifest.containers.clone_from(&record.containers);
    manifest.images.clone_from(&record.images);
    manifest.networks.clone_from(&record.networks);
    manifest.volumes.clone_from(&record.volumes);
    manifest.filesystem.clone_from(&record.filesystem);
    manifest.workspace_environment.clone_from(&record.workspace_environment);
    manifest.pane_providers.clone_from(&record.pane_providers);
    manifest
}

/// Puts one stored record under the policy, in the state it was stored in.
fn enrol(installation: &mut Installation, record: &Record) -> Result<(), Objection> {
    installation.install_resource_scoped(
        &described(record),
        &record.image_digest,
        &record.granted,
        &record.containers,
        &record.images,
        &record.networks,
        &record.volumes,
        &record.filesystem,
        &record.workspace_environment,
        record.installed_at,
    )?;
    if record.enabled {
        installation.enable(&record.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{described, Refusal, Roster};
    use hl_extension::{Capability, ExtensionName, Grant, Manifest, Stage};
    use hl_ws::storage::{Directory, Key, Storage};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    #[derive(Clone)]
    struct RefuseFaultClear {
        inner: Directory,
        refuse: Arc<AtomicBool>,
    }

    impl Storage for RefuseFaultClear {
        type Error = hl_ws::storage::Error;

        fn put(&self, key: &Key, bytes: &[u8]) -> Result<(), Self::Error> {
            self.inner.put(key, bytes)
        }

        fn get(&self, key: &Key) -> Result<Vec<u8>, Self::Error> {
            self.inner.get(key)
        }

        fn list(&self, prefix: Option<&Key>) -> Result<Vec<Key>, Self::Error> {
            self.inner.list(prefix)
        }

        fn list_until(&self, prefix: Option<&Key>, deadline: Instant) -> Result<Vec<Key>, Self::Error> {
            self.inner.list_until(prefix, deadline)
        }

        fn remove(&self, key: &Key) -> Result<(), Self::Error> {
            if key.as_str().starts_with(super::super::state::FAULT_PREFIX) && self.refuse.swap(false, Ordering::AcqRel)
            {
                return Err(std::io::Error::other("injected fault-marker refusal").into());
            }
            self.inner.remove(key)
        }

        fn remove_until(&self, key: &Key, deadline: Instant) -> Result<(), Self::Error> {
            self.inner.remove_until(key, deadline)
        }
    }

    fn manifest(name: &str, capabilities: &[Capability]) -> Manifest {
        Manifest {
            containers: hl_extension::ContainerGrant::default(),
            images: hl_extension::ImageGrant::default(),
            networks: hl_extension::NetworkGrant::default(),
            volumes: hl_extension::VolumeGrant::default(),
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
            filesystem: hl_extension::FilesystemGrant::default(),
            workspace_environment: hl_extension::WorkspaceEnvironmentGrant::default(),
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
    fn exact_workspace_environment_consent_survives_reopen() {
        let temporary = tempfile::tempdir().unwrap();
        let mut asked = manifest("sample", &[Capability::WorkspaceEnvironmentRead]);
        let exact = hl_extension::WorkspaceEnvironmentGrant {
            read: vec![hl_extension::WorkspaceEnvironmentSelector::Exact {
                workspace: "dev".into(),
                name: "PGPASSWORD".into(),
            }],
            write: Vec::new(),
        };
        asked.workspace_environment = exact.clone();
        let mut roster = opened(temporary.path());
        roster
            .register_resource_scoped(
                &asked,
                "sha256:exact",
                &asked.capabilities,
                &asked.containers,
                &asked.images,
                &asked.networks,
                &asked.volumes,
                &asked.filesystem,
                &exact,
                7,
            )
            .unwrap();
        drop(roster);

        let reopened = opened(temporary.path());
        let record = reopened.installation.records().next().unwrap();
        assert_eq!(record.workspace_environment, exact);
        assert_eq!(described(record).workspace_environment, exact);
    }

    #[test]
    fn launch_declaration_survives_reopen_without_widening_consent() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut asked = manifest("sample", &[Capability::ContainerRead, Capability::Interface]);
        asked.entrypoint = Some(vec!["/opt/sample/bin/serve".to_owned(), "--socket".to_owned()]);
        asked.resources = hl_extension::Resources {
            memory_mb: 384,
            cpus: 2,
            process_count: 41,
        };
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:aaaa", &Grant::new([Capability::Interface]), 7)
            .expect("registered");

        let reopened = opened(temporary.path());
        let record = reopened.installation.record(&asked.name).expect("reopened record");
        let restored = described(record);

        assert_eq!(restored.entrypoint, asked.entrypoint);
        assert_eq!(restored.resources, asked.resources);
        assert!(restored.capabilities.holds(Capability::Interface));
        assert!(
            !restored.capabilities.holds(Capability::ContainerRead),
            "persisted requested capabilities cannot widen the recorded grant"
        );
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
    fn disabling_a_host_fault_survives_reopen_as_standby() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");
        roster.enable(&asked.name).expect("enabled");
        roster.fault(&asked.name, 6).expect("fault persisted");

        roster.disable(&asked.name).expect("disabled");

        assert_eq!(roster.stage(&asked.name), Stage::Standby);
        assert_eq!(opened(temporary.path()).stage(&asked.name), Stage::Standby);
    }

    #[test]
    fn failed_fault_clear_rolls_back_the_durable_disable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let refuse = Arc::new(AtomicBool::new(false));
        let storage = Directory::open(temporary.path()).expect("storage");
        let mut roster = Roster::open(RefuseFaultClear {
            inner: storage,
            refuse: Arc::clone(&refuse),
        })
        .expect("roster");
        let asked = manifest("sample", &[Capability::Interface]);
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");
        roster.enable(&asked.name).expect("enabled");
        roster.fault(&asked.name, 6).expect("fault persisted");
        refuse.store(true, Ordering::Release);

        assert!(roster.disable(&asked.name).is_err());

        assert_eq!(roster.stage(&asked.name), Stage::Fault { restarts: 6 });
        assert_eq!(
            opened(temporary.path()).stage(&asked.name),
            Stage::Fault { restarts: 6 },
            "a reopen sees the former enabled fault, not a half-disable"
        );
    }

    #[test]
    fn failed_fault_clear_does_not_partially_uninstall_the_extension() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let refuse = Arc::new(AtomicBool::new(false));
        let storage = Directory::open(temporary.path()).expect("storage");
        let mut roster = Roster::open(RefuseFaultClear {
            inner: storage,
            refuse: Arc::clone(&refuse),
        })
        .expect("roster");
        let asked = manifest("sample", &[Capability::Interface]);
        roster
            .register(&asked, "sha256:aaaa", &asked.capabilities, 7)
            .expect("registered");
        roster.enable(&asked.name).expect("enabled");
        roster.fault(&asked.name, 6).expect("fault persisted");
        refuse.store(true, Ordering::Release);

        assert!(roster.remove(&asked.name).is_err());

        assert_eq!(roster.stage(&asked.name), Stage::Fault { restarts: 6 });
        let reopened = opened(temporary.path());
        assert_eq!(
            reopened.stage(&asked.name),
            Stage::Fault { restarts: 6 },
            "a refused removal retains the durable record and crash marker"
        );
        assert_eq!(reopened.entries()[0].image_digest, "sha256:aaaa");
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
        roster
            .register(&asked, "sha256:new", &asked.capabilities, 7)
            .expect("registered");
        assert!(roster.remove_if_digest(&asked.name, "sha256:old").is_err());
        assert_eq!(roster.entries()[0].image_digest, "sha256:new");
        assert_eq!(opened(temporary.path()).entries()[0].image_digest, "sha256:new");
    }

    #[test]
    fn stale_roster_removal_cannot_delete_a_reinstalled_digest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut installer = opened(temporary.path());
        installer
            .register(&asked, "sha256:old", &asked.capabilities, 7)
            .expect("old install");
        let mut stale = opened(temporary.path());

        installer.remove_if_digest(&asked.name, "sha256:old").expect("old removal");
        installer
            .register(&asked, "sha256:new", &asked.capabilities, 8)
            .expect("replacement install");

        assert!(stale.remove_if_digest(&asked.name, "sha256:old").is_err());
        let persisted = opened(temporary.path()).entries();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].image_digest, "sha256:new");
    }

    #[test]
    fn stale_roster_cannot_enable_and_restore_a_replaced_grant() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let old = manifest("sample", &[Capability::Interface]);
        let mut installer = opened(temporary.path());
        installer
            .register(&old, "sha256:old", &old.capabilities, 7)
            .expect("old install");
        let mut stale = opened(temporary.path());

        installer.remove_if_digest(&old.name, "sha256:old").expect("old removal");
        let replacement = manifest("sample", &[Capability::ContainerRead]);
        installer
            .register(&replacement, "sha256:new", &replacement.capabilities, 8)
            .expect("replacement install");

        assert!(stale.enable_if_digest(&old.name, "sha256:old").is_err());
        let persisted = opened(temporary.path()).entries();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].image_digest, "sha256:new");
        assert_eq!(persisted[0].granted, replacement.capabilities);
        assert_eq!(persisted[0].stage, Stage::Standby);
    }

    #[test]
    fn late_fault_from_removed_sidecar_cannot_fault_its_replacement() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut installer = opened(temporary.path());
        installer
            .register(&asked, "sha256:old", &asked.capabilities, 7)
            .expect("old install");
        installer.enable(&asked.name).expect("old enabled");
        let mut old_host = opened(temporary.path());

        installer.remove_if_digest(&asked.name, "sha256:old").expect("old removal");
        let replacement = manifest("sample", &[Capability::ContainerRead]);
        installer
            .register(&replacement, "sha256:new", &replacement.capabilities, 8)
            .expect("replacement install");
        installer.enable(&replacement.name).expect("replacement enabled");

        assert!(old_host
            .fault_if_digest(&asked.name, "sha256:old", 5)
            .is_err());
        let persisted = opened(temporary.path()).entries();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].image_digest, "sha256:new");
        assert_eq!(persisted[0].granted, replacement.capabilities);
        assert_eq!(persisted[0].stage, Stage::Duty);
    }

    #[test]
    fn stale_state_change_cannot_control_a_reinstalled_digest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut roster = opened(temporary.path());
        roster
            .register(&asked, "sha256:new", &asked.capabilities, 7)
            .expect("registered");
        assert!(roster.enable_if_digest(&asked.name, "sha256:old").is_err());
        assert!(roster.disable_if_digest(&asked.name, "sha256:old").is_err());
        assert_eq!(roster.stage(&asked.name), Stage::Standby);
    }

    #[test]
    fn delayed_enable_loses_to_a_concurrent_reinstallation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut initial = opened(temporary.path());
        initial
            .register(&asked, "sha256:old", &asked.capabilities, 7)
            .expect("registered");
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
        assert!(roster
            .lock()
            .unwrap()
            .enable_if_digest(&asked.name, "sha256:old")
            .is_err());
        worker.join().unwrap();
        assert_eq!(opened(temporary.path()).stage(&asked.name), Stage::Standby);
    }

    #[test]
    fn delayed_removal_loses_to_a_concurrent_reinstallation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let asked = manifest("sample", &[Capability::Interface]);
        let mut initial = opened(temporary.path());
        initial
            .register(&asked, "sha256:old", &asked.capabilities, 7)
            .expect("registered");
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
        assert!(roster
            .lock()
            .unwrap()
            .remove_if_digest(&asked.name, "sha256:old")
            .is_err());
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
    fn stale_rosters_cannot_both_install_one_extension_name() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first_manifest = manifest("sample", &[Capability::Interface]);
        let second_manifest = manifest("sample", &[Capability::ContainerRead]);
        let mut first = opened(temporary.path());
        let mut second = opened(temporary.path());

        first
            .register(&first_manifest, "sha256:first", &first_manifest.capabilities, 7)
            .expect("winning registration");
        assert!(matches!(
            second.register(&second_manifest, "sha256:second", &second_manifest.capabilities, 8),
            Err(Refusal::Policy(_))
        ));

        let persisted = opened(temporary.path()).entries();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].image_digest, "sha256:first");
        assert!(persisted[0].granted.holds(Capability::Interface));
        assert!(!persisted[0].granted.holds(Capability::ContainerRead));
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
