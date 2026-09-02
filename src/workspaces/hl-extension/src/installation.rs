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
}

impl std::fmt::Display for Objection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Presence(name) => write!(formatter, "{name} is already installed"),
            Self::Absence(name) => write!(formatter, "{name} is not installed"),
            Self::Digest => formatter.write_str("an image digest is required to record a grant"),
        }
    }
}

impl std::error::Error for Objection {}

/// The consent standing of an update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Consent {
    /// The recorded grant already covers what the updated manifest asks for,
    /// so nothing has to be put to a person.
    Standing,
    /// The update asks for capabilities the record does not hold. The extension
    /// keeps the old, narrower grant until these are agreed to separately.
    Requirement {
        /// Exactly the capabilities that are new, in a stable order.
        additional: Vec<Capability>,
    },
}

impl Consent {
    /// Whether a person has to be asked before the update gets what it wants.
    #[must_use]
    pub const fn is_requirement(&self) -> bool {
        matches!(self, Self::Requirement { .. })
    }
}

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

/// Restart bookkeeping for one record. Not persisted: a host restart is a
/// fresh start, and a fault a person never saw should not outlive the process
/// that produced it.
#[derive(Clone, Copy, Debug, Default)]
struct Restarts {
    count: u32,
    window_start: i64,
    faulted: bool,
}

struct Entry {
    record: Record,
    restarts: Restarts,
}

/// Every extension recorded for one workspace.
#[derive(Default)]
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
                granted,
                enabled: false,
                installed_at: at,
                pane_providers: manifest.pane_providers.clone(),
            },
            restarts: Restarts::default(),
        });
        Ok(&entry.record)
    }

    /// Points an existing record at a new image.
    ///
    /// The recorded grant is narrowed to what the updated manifest still asks
    /// for and never widened. When the update asks for more, the extension
    /// keeps running on the old grant and the caller is handed
    /// [`Consent::Requirement`] naming exactly what is new, so the choice
    /// reaches a person instead of being made for them.
    ///
    /// The restart count is cleared, because a new image has not failed yet.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under the
    /// manifest's name, and `Objection::Digest` when the digest is empty.
    pub fn reinstall(&mut self, manifest: &Manifest, digest: &str, at: i64) -> Result<Consent, Objection> {
        if digest.is_empty() {
            return Err(Objection::Digest);
        }
        let entry = self
            .entries
            .get_mut(&manifest.name)
            .ok_or_else(|| Objection::Absence(manifest.name.clone()))?;
        let additional = entry.record.granted.missing(&manifest.capabilities);
        entry.record.granted = entry.record.granted.intersect(&manifest.capabilities);
        digest.clone_into(&mut entry.record.image_digest);
        entry.record.installed_at = at;
        entry.record.pane_providers.clone_from(&manifest.pane_providers);
        entry.restarts = Restarts::default();
        if additional.is_empty() {
            return Ok(Consent::Standing);
        }
        Ok(Consent::Requirement { additional })
    }

    /// Widens a record to a grant a person has just agreed to.
    ///
    /// This is the only widening path in the crate, and it takes the consent as
    /// an argument so that it cannot be reached except from a prompt's answer.
    /// The result is still an intersection with what the manifest asks for.
    ///
    /// # Errors
    /// Returns `Objection::Absence` when nothing is recorded under the
    /// manifest's name.
    pub fn consent(&mut self, manifest: &Manifest, consented: &Grant) -> Result<&Record, Objection> {
        let entry = self
            .entries
            .get_mut(&manifest.name)
            .ok_or_else(|| Objection::Absence(manifest.name.clone()))?;
        let agreed = manifest.capabilities.intersect(consented);
        entry.record.granted = Grant::new(entry.record.granted.iter().chain(agreed.iter()));
        Ok(&entry.record)
    }

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
    /// This is the only exit from [`Stage::Fault`], so a crash loop cannot
    /// resolve itself into a state nobody was told about.
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
