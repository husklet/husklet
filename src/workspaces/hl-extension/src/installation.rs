//! What a host records about an installed extension, and the policy that moves
//! it between stages.
//!
//! This is state and policy only. Nothing here touches a filesystem, starts a
//! container, or reads a clock: every moment is passed in, so the whole
//! lifecycle — including the restart window — is exercised without a runtime.
//!
//! The rule the rest of the design leans on is that a grant only ever narrows.
//! An updated manifest asking for more keeps the recorded grant and reports
//! what is missing; widening requires a person, and there is no code path that
//! does it on their behalf.

use std::collections::BTreeMap;

use crate::capability::{Capability, Grant};
use crate::manifest::{ExtensionName, Manifest};

/// What a host persists per workspace for one extension.
///
/// The digest is recorded alongside the grant because consent was given for a
/// specific image. A different digest is a different program, so it re-enters
/// the consent check rather than inheriting the old answer.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// Identity of the extension, and the key it is stored under.
    pub name: ExtensionName,
    /// Digest of the image the grant was given for.
    pub image_digest: String,
    /// Manifest version consented to for this digest. Empty only for records
    /// written before versions were persisted.
    #[serde(default)]
    pub version: String,
    /// Exactly what the person agreed to, never what was asked for.
    pub granted: Grant,
    /// Whether the sidecar should be running.
    pub enabled: bool,
    /// When the record was first written, in milliseconds since the epoch,
    /// supplied by the caller because this crate has no clock.
    pub installed_at: i64,
    /// Pane-provider catalogue consented to with this exact image digest.
    #[serde(default)]
    pub pane_providers: Vec<crate::manifest::PaneProvider>,
}

/// Where an extension stands right now.
///
/// `Fault` is separate from a record that is merely not enabled, because the
/// two mean opposite things to a person: one is a choice they made, the other
/// is a failure they have to be shown and offered a retry for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Nothing is recorded under this name.
    Vacancy,
    /// A record exists and the sidecar is meant to stay down.
    Standby,
    /// A record exists and the sidecar is meant to be running.
    Duty,
    /// Restarts exceeded the attempt limit inside the window. Terminal until
    /// [`Installation::retry`] is called.
    Fault {
        /// Restarts counted in the window that ended in the fault.
        restarts: u32,
    },
}

impl Stage {
    /// Whether this stage is the terminal fault, which a caller must not
    /// present as a plain disable.
    #[must_use]
    pub const fn is_fault(self) -> bool {
        matches!(self, Self::Fault { .. })
    }
}

/// Why an operation on the installation was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Objection {
    /// A record already exists under this name; the caller wanted
    /// [`Installation::reinstall`].
    Presence(ExtensionName),
    /// No record exists under this name.
    Absence(ExtensionName),
    /// The image digest was empty, so the grant could not be tied to an image.
    Digest,
    /// The prepared update no longer describes the installed image.
    Changed(ExtensionName),
    /// The explicit answer did not cover every newly requested capability.
    Consent(Vec<Capability>),
}

impl std::fmt::Display for Objection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Presence(name) => write!(formatter, "{name} is already installed"),
            Self::Absence(name) => write!(formatter, "{name} is not installed"),
            Self::Digest => formatter.write_str("an image digest is required to record a grant"),
            Self::Changed(name) => write!(formatter, "{name} changed while its update was pending"),
            Self::Consent(capabilities) => write!(formatter, "update consent is missing {capabilities:?}"),
        }
    }
}

/// An inspected update that has not changed installed state or runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Update {
    /// Installed name both records must share.
    pub name: ExtensionName,
    /// Digest still installed while the prompt is open.
    pub current_digest: String,
    /// Version still installed while the prompt is open.
    pub current_version: String,
    /// Digest inspected from the candidate image.
    pub candidate_digest: String,
    /// Version inspected from the candidate manifest.
    pub candidate_version: String,
    /// Capabilities the candidate newly requests, in stable order.
    pub additional: Vec<Capability>,
    /// Previously granted capabilities the candidate no longer requests.
    pub removed: Vec<Capability>,
    manifest: Manifest,
}

/// Why committing an inspected update did not replace anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateFailure<E> {
    /// State or consent changed before replacement began.
    Refused(Objection),
    /// The host could not atomically replace the old runtime.
    Replacement(E),
}

impl std::error::Error for Objection {}

/// What a host should do with a sidecar that just stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    /// Start it again after waiting this long.
    Backoff {
        /// Which restart this is inside the current window, counting from one.
        attempt: u32,
        /// How long to wait first, in milliseconds.
        delay_ms: u64,
    },
    /// Stop restarting and show the fault. The extension stays here until a
    /// person retries it.
    Fault {
        /// Restarts counted in the window.
        restarts: u32,
    },
}

/// Restart bookkeeping for one record. This shape is not serialized in a
/// consent [`Record`]; an application may restore a host-observed terminal
/// fault from separate durable lifecycle state.
#[derive(Clone, Copy, Debug, Default)]
struct Restarts {
    count: u32,
    window_start: i64,
    faulted: bool,
}

#[derive(Clone)]
struct Entry {
    record: Record,
    restarts: Restarts,
}

/// Every extension recorded for one workspace.
#[derive(Clone, Default)]
pub struct Installation {
    entries: BTreeMap<ExtensionName, Entry>,
}

impl std::fmt::Debug for Installation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Installation")
            .field("installed", &self.entries.len())
            .finish()
    }
}

impl Installation {
    /// Restarts tolerated inside one window before the extension faults. Five
    /// is enough to ride out a transient dependency coming up late, and few
    /// enough that a crash loop is surfaced within a few seconds.
    pub const ATTEMPT_LIMIT: u32 = 5;
    /// How long a run has to last for the restart count to start over, in
    /// milliseconds. A sidecar that stayed up a full minute was not crash
    /// looping, so its next failure is judged on its own.
    pub const WINDOW_MS: i64 = 60_000;
    /// Delay before the first restart, in milliseconds. Doubles per attempt.
    pub const BACKOFF_BASE_MS: u64 = 500;
    /// Longest delay the backoff reaches, in milliseconds. The cap matters
    /// because the attempt limit is small: without it the last wait would be
    /// long enough that a person reads it as a hang rather than a retry.
    pub const BACKOFF_CAP_MS: u64 = 8_000;

    /// An installation with nothing recorded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many extensions are recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The record stored under `name`, if there is one.
    #[must_use]
    pub fn record(&self, name: &ExtensionName) -> Option<&Record> {
        self.entries.get(name).map(|entry| &entry.record)
    }

    /// Every record, ordered by name.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.entries.values().map(|entry| &entry.record)
    }

    /// Where `name` stands.
    #[must_use]
    pub fn stage(&self, name: &ExtensionName) -> Stage {
        let Some(entry) = self.entries.get(name) else {
            return Stage::Vacancy;
        };
        if entry.restarts.faulted {
            return Stage::Fault {
                restarts: entry.restarts.count,
            };
        }
        if entry.record.enabled {
            return Stage::Duty;
        }
        Stage::Standby
    }

    /// Records a first install, granting the intersection of what the manifest
    /// asks for and what was consented to.
    ///
    /// The intersection is taken rather than the consent alone so that consent
    /// carried over from a wider prompt cannot grant something this manifest
    /// never declared, and never the request alone so that an unasked-for
    /// capability cannot arrive with an image update.
    ///
    /// `at` is the install moment in milliseconds since the epoch; this crate
    /// has no clock of its own.
    ///
    /// # Errors
    /// Returns `Objection::Presence` when a record already exists, and
    /// `Objection::Digest` when the digest is empty.
    pub fn install(
        &mut self,
        manifest: &Manifest,
        digest: &str,
        consented: &Grant,
        at: i64,
    ) -> Result<&Record, Objection> {
        if digest.is_empty() {
            return Err(Objection::Digest);
        }
        if self.entries.contains_key(&manifest.name) {
            return Err(Objection::Presence(manifest.name.clone()));
        }
        let granted = manifest.capabilities.intersect(consented);
        // The name is vacant, checked above, so this always inserts.
        let entry = self.entries.entry(manifest.name.clone()).or_insert_with(|| Entry {
            record: Record {
                name: manifest.name.clone(),
                image_digest: digest.to_owned(),
                version: manifest.version.clone(),
                granted,
                enabled: false,
                installed_at: at,
                pane_providers: manifest.pane_providers.clone(),
            },
            restarts: Restarts::default(),
        });
        Ok(&entry.record)
    }

    /// Inspects an update without changing the installed record or runtime.
    ///
    /// Capability additions and removals are returned for an explicit prompt;
    /// the old record remains authoritative until [`Self::commit_update`].
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under the
    /// manifest's name, and `Objection::Digest` when the digest is empty.
    pub fn prepare_update(&self, manifest: &Manifest, digest: &str) -> Result<Update, Objection> {
        if digest.is_empty() {
            return Err(Objection::Digest);
        }
        let entry = self
            .entries
            .get(&manifest.name)
            .ok_or_else(|| Objection::Absence(manifest.name.clone()))?;
        Ok(Update {
            name: manifest.name.clone(),
            current_digest: entry.record.image_digest.clone(),
            current_version: entry.record.version.clone(),
            candidate_digest: digest.to_owned(),
            candidate_version: manifest.version.clone(),
            additional: entry.record.granted.missing(&manifest.capabilities),
            removed: manifest.capabilities.missing(&entry.record.granted),
            manifest: manifest.clone(),
        })
    }

    /// Commits an explicitly accepted update after its runtime replacement succeeds.
    ///
    /// This is the only widening path in the crate, and it takes the selected
    /// consent as an argument so that it cannot be reached except from a
    /// prompt's answer. Optional additions left unselected are not granted;
    /// interface authority remains mandatory for an authored interface.
    /// The result is still an intersection with what the manifest asks for.
    /// `replace` must be atomic from the host's perspective and leave the old
    /// runtime in service on error; the record changes only after success.
    ///
    /// # Errors
    /// Returns a refusal when state changed or consent is incomplete, or the
    /// replacement failure without changing the record.
    pub fn commit_update<E>(
        &mut self,
        update: Update,
        consented: &Grant,
        at: i64,
        replace: impl FnOnce(&Record, &Record) -> Result<(), E>,
    ) -> Result<&Record, UpdateFailure<E>> {
        let entry = self
            .entries
            .get_mut(&update.name)
            .ok_or_else(|| UpdateFailure::Refused(Objection::Absence(update.name.clone())))?;
        if entry.record.image_digest != update.current_digest {
            return Err(UpdateFailure::Refused(Objection::Changed(update.name)));
        }
        let granted =
            Grant::new(entry.record.granted.iter().chain(consented.iter())).intersect(&update.manifest.capabilities);
        let required = Grant::new(
            (update.manifest.interface.is_some() || !update.manifest.pane_providers.is_empty())
                .then_some(Capability::Interface),
        );
        let missing = granted.missing(&required);
        if !missing.is_empty() {
            return Err(UpdateFailure::Refused(Objection::Consent(missing)));
        }
        let next = Record {
            name: update.name,
            image_digest: update.candidate_digest,
            version: update.candidate_version,
            granted,
            enabled: entry.record.enabled,
            installed_at: at,
            pane_providers: update.manifest.pane_providers,
        };
        replace(&entry.record, &next).map_err(UpdateFailure::Replacement)?;
        entry.record = next;
        entry.restarts = Restarts::default();
        Ok(&entry.record)
    }

    /// Cancels a prepared update. Ownership makes cancellation explicit while
    /// leaving both the record and runtime untouched.
    pub fn cancel_update(&self, _update: Update) {}

    /// Marks a record as one whose sidecar should run. Leaves the grant and the
    /// digest untouched.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under `name`.
    pub fn enable(&mut self, name: &ExtensionName) -> Result<&Record, Objection> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| Objection::Absence(name.clone()))?;
        entry.record.enabled = true;
        Ok(&entry.record)
    }

    /// Marks a record as one whose sidecar should stay down. The grant survives
    /// so re-enabling does not ask again.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under `name`.
    pub fn disable(&mut self, name: &ExtensionName) -> Result<&Record, Objection> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| Objection::Absence(name.clone()))?;
        entry.record.enabled = false;
        // A deliberate disable is also an explicit answer to a crash loop.
        // Retaining the terminal marker would present a stopped extension as
        // faulted forever and make a later enable indistinguishable from retry.
        entry.restarts = Restarts::default();
        Ok(&entry.record)
    }

    /// Forgets a record entirely and returns it.
    ///
    /// Nothing is retained, so a later install of the same name starts from an
    /// empty grant. A grant that outlived its uninstall would be consent a
    /// person believes they withdrew.
    pub fn uninstall(&mut self, name: &ExtensionName) -> Option<Record> {
        self.entries.remove(name).map(|entry| entry.record)
    }

    /// Accounts for a sidecar that just stopped and says what to do next.
    ///
    /// `at` is the moment it stopped, in milliseconds since the epoch. Restarts
    /// further apart than [`Installation::WINDOW_MS`] start the count over.
    /// Passing [`Installation::ATTEMPT_LIMIT`] leaves the extension in
    /// [`Stage::Fault`], which is user-visible and stays until
    /// [`Installation::retry`]; it is deliberately not a quiet disable, because
    /// a person who never chose it would find it off with no explanation.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under `name`.
    pub fn restarted(&mut self, name: &ExtensionName, at: i64) -> Result<Disposition, Objection> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| Objection::Absence(name.clone()))?;
        let lapsed = at.saturating_sub(entry.restarts.window_start) > Self::WINDOW_MS;
        if entry.restarts.count == 0 || (lapsed && !entry.restarts.faulted) {
            entry.restarts = Restarts {
                count: 0,
                window_start: at,
                faulted: false,
            };
        }
        entry.restarts.count += 1;
        if entry.restarts.count < Self::ATTEMPT_LIMIT && !entry.restarts.faulted {
            return Ok(Disposition::Backoff {
                attempt: entry.restarts.count,
                delay_ms: Self::backoff_ms(entry.restarts.count),
            });
        }
        entry.restarts.faulted = true;
        Ok(Disposition::Fault {
            restarts: entry.restarts.count,
        })
    }

    /// Clears a fault at a person's request and puts the sidecar back on duty.
    ///
    /// Alongside an explicit disable, this is an exit from [`Stage::Fault`],
    /// so a crash loop cannot resolve itself into a state nobody was told about.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under `name`.
    pub fn retry(&mut self, name: &ExtensionName) -> Result<&Record, Objection> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| Objection::Absence(name.clone()))?;
        entry.restarts = Restarts::default();
        entry.record.enabled = true;
        Ok(&entry.record)
    }

    /// Restores a fault observed by the live host into this policy.
    ///
    /// The host owns restart timing because it observes exits; a roster owns
    /// durable presentation because it survives page and process rebuilds.
    /// This is the narrow bridge between them: it cannot start, stop, remove,
    /// or widen the grant of an extension.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under `name`.
    pub fn fault(&mut self, name: &ExtensionName, restarts: u32) -> Result<&Record, Objection> {
        let entry = self
            .entries
            .get_mut(name)
            .ok_or_else(|| Objection::Absence(name.clone()))?;
        entry.restarts = Restarts {
            count: restarts,
            window_start: 0,
            faulted: true,
        };
        Ok(&entry.record)
    }

    /// Delay before restart number `attempt`, doubling from the base and held
    /// at the cap.
    fn backoff_ms(attempt: u32) -> u64 {
        let doubled = Self::BACKOFF_BASE_MS.checked_shl(attempt.saturating_sub(1));
        doubled.unwrap_or(Self::BACKOFF_CAP_MS).min(Self::BACKOFF_CAP_MS)
    }
}

/// What an install prompt has to say about a grant.
///
/// The execution line is the one that matters. An extension holding
/// [`Capability::ContainerControl`] or [`Capability::TerminalControl`] can run
/// programs of its choosing inside the workspace, and the isolation on offer is
/// the workspace boundary, not a sandbox around the extension. A prompt that
/// leaves that implicit is telling a person something untrue by omission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Summary {
    /// Whether the grant amounts to running code inside the workspace.
    pub execution: bool,
    /// The capabilities that can change something, in a stable order.
    pub mutations: Vec<Capability>,
    /// The capabilities that only observe, in a stable order.
    pub observations: Vec<Capability>,
}

impl Summary {
    /// The sentence a prompt must show when the grant permits execution, and
    /// the reason it names the workspace rather than a sandbox.
    /// What an install prompt must say about `grant`.
    #[must_use]
    pub fn of(grant: &Grant) -> Self {
        Self {
            execution: grant.executes(),
            mutations: grant.iter().filter(|held| held.mutates()).collect(),
            observations: grant.iter().filter(|held| !held.mutates()).collect(),
        }
    }

    pub const EXECUTION_NOTICE: &'static str =
        "This extension can run programs inside this workspace. It is isolated from the rest of \
         your machine by the workspace, not from the workspace itself.";
}

impl std::fmt::Display for Summary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.execution {
            writeln!(formatter, "{}", Self::EXECUTION_NOTICE)?;
        }
        for capability in self.mutations.iter().chain(&self.observations) {
            writeln!(formatter, "{}", capability.as_str())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Capability, Disposition, Grant, Installation, Stage, Summary};
    use crate::manifest::{ExtensionName, Manifest};

    fn manifest(capabilities: &[Capability]) -> Manifest {
        Manifest {
            name: ExtensionName::new("sample").expect("name"),
            display_name: "Sample".to_owned(),
            version: "1.0.0".to_owned(),
            protocol: hl_rpc::PROTOCOL,
            capabilities: Grant::new(capabilities.iter().copied()),
            entrypoint: None,
            activation: crate::manifest::Activation::default(),
            interface: None,
            pane_providers: Vec::new(),
            resources: crate::manifest::Resources::default(),
            filesystem_roots: Vec::new(),
        }
    }

    #[test]
    fn an_install_records_the_intersection() {
        let mut installation = Installation::new();
        let manifest = manifest(&[Capability::ContainerRead, Capability::ContainerControl]);
        let record = installation
            .install(&manifest, "sha256:a", &Grant::new([Capability::ContainerRead]), 10)
            .expect("installed");

        assert!(record.granted.holds(Capability::ContainerRead));
        assert!(!record.granted.holds(Capability::ContainerControl));
    }

    #[test]
    fn a_second_install_is_refused_rather_than_overwriting() {
        let mut installation = Installation::new();
        let manifest = manifest(&[Capability::ContainerRead]);
        installation
            .install(&manifest, "sha256:a", &manifest.capabilities, 10)
            .expect("installed");

        assert!(installation
            .install(&manifest, "sha256:b", &manifest.capabilities, 20)
            .is_err());
    }

    #[test]
    fn an_observed_host_fault_is_structured_and_retry_is_its_only_reset() {
        let mut installation = Installation::new();
        let manifest = manifest(&[Capability::Interface]);
        installation
            .install(&manifest, "sha256:a", &manifest.capabilities, 10)
            .expect("installed");
        installation.enable(&manifest.name).expect("enabled");

        installation.fault(&manifest.name, 7).expect("fault recorded");
        assert_eq!(installation.stage(&manifest.name), Stage::Fault { restarts: 7 });

        installation.retry(&manifest.name).expect("retried");
        assert_eq!(installation.stage(&manifest.name), Stage::Duty);
    }

    #[test]
    fn disabling_a_fault_is_an_explicit_standby_decision() {
        let mut installation = Installation::new();
        let manifest = manifest(&[Capability::Interface]);
        installation
            .install(&manifest, "sha256:a", &manifest.capabilities, 10)
            .expect("installed");
        installation.enable(&manifest.name).expect("enabled");
        installation.fault(&manifest.name, 7).expect("fault recorded");

        installation.disable(&manifest.name).expect("disabled");

        assert_eq!(installation.stage(&manifest.name), Stage::Standby);
        assert!(!installation.record(&manifest.name).expect("record").enabled);
    }

    #[test]
    fn a_fault_is_not_reported_as_a_standby() {
        let mut installation = Installation::new();
        let manifest = manifest(&[Capability::ContainerRead]);
        installation
            .install(&manifest, "sha256:a", &manifest.capabilities, 0)
            .expect("installed");
        installation.enable(&manifest.name).expect("enabled");

        for attempt in 1..=Installation::ATTEMPT_LIMIT {
            let disposition = installation
                .restarted(&manifest.name, i64::from(attempt))
                .expect("counted");
            let last = attempt == Installation::ATTEMPT_LIMIT;
            assert_eq!(last, matches!(disposition, Disposition::Fault { .. }));
        }

        assert_eq!(
            installation.stage(&manifest.name),
            Stage::Fault {
                restarts: Installation::ATTEMPT_LIMIT
            }
        );
        assert!(installation.stage(&manifest.name).is_fault());
    }

    #[test]
    fn the_backoff_is_held_at_the_cap() {
        for attempt in 1..64 {
            assert!(Installation::backoff_ms(attempt) <= Installation::BACKOFF_CAP_MS);
        }
        assert_eq!(Installation::backoff_ms(1), Installation::BACKOFF_BASE_MS);
    }

    #[test]
    fn the_summary_names_execution_plainly() {
        let summary = Summary::of(&Grant::new([Capability::TerminalControl]));
        assert!(summary.execution);
        assert!(summary.to_string().contains(Summary::EXECUTION_NOTICE));

        let reading = Summary::of(&Grant::new([Capability::ContainerRead]));
        assert!(!reading.execution);
        assert!(!reading.to_string().contains("run programs"));
    }
}
