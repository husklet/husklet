//! The listings a workspace pushes to a subscriber.
//!
//! Routing, credit, and the bulk byte streams are `hl-rpc`'s; what is in a
//! listing is this domain's, and is here.

use hl_rpc::{Coding, Frame};

use crate::port::{
    ContainerSummary, ExecutionList, ExtensionSummary, ImagePullChange, ImageSummary, NetworkSummary, TabSummary,
    VolumeSummary,
};
use crate::request::Topic;

/// What produced a pane notification. Contents remain behind their separate
/// terminal-output and pane-semantic-read grants.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneChangeKind {
    Terminal,
    Surface,
    Native,
}

/// Bounded invalidation metadata. Consumers fetch a fresh typed snapshot after
/// receiving this rather than polling or accepting pushed pane contents.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneChange {
    pub slot: String,
    pub kind: PaneChangeKind,
    pub revision: u64,
    pub generation: u64,
    pub coalesced: u64,
}

/// Bounded acquisition invalidation metadata. Candidate contents remain behind
/// an explicit status read so frequent progress never fills the event outbox.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionAcquisitionChange {
    pub job: String,
    pub revision: u64,
    pub state: String,
    pub coalesced: u64,
}

/// One successful workspace mutation. Revisions are monotonically increasing
/// within the host process and let consumers discard stale/coalesced notices.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceLifecycleChange {
    pub workspace: String,
    pub action: WorkspaceLifecycleAction,
    pub revision: u64,
    pub coalesced: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLifecycleAction {
    Create,
    Update,
    Remove,
    Start,
    Stop,
    Restart,
}

/// Window-level activity visible to an extension holding `workspace-events`.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkspaceEvent {
    Key {
        key: String,
        modifiers: Vec<String>,
        pressed: bool,
    },
    Focus {
        active: bool,
    },
    Pointer {
        phase: PointerPhase,
        slot: String,
        generation: u64,
        x: f64,
        y: f64,
        button: Option<u32>,
        modifiers: Vec<String>,
        delta_x: Option<f64>,
        delta_y: Option<f64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerPhase {
    Move,
    Enter,
    Leave,
    Press,
    Release,
    Click,
    Context,
    Scroll,
}

/// A bounded observation batch. `dropped` makes overload visible to consumers.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceEventBatch {
    pub events: Vec<WorkspaceEvent>,
    pub dropped: u64,
}

/// Which channel each followed topic is delivered on.
pub type Subscriptions = hl_rpc::Subscriptions<Topic>;

/// The whole current listing behind one topic.
///
/// Every variant carries the complete listing rather than the change that
/// produced it, and that is a requirement rather than a convenience. A
/// subscription coalesces: when the subscriber stops returning credit, the
/// outbox replaces the queued value for a topic with the newer one, so the
/// subscriber may receive one value where the host emitted a thousand. A delta
/// would then describe a change from a state that was dropped on the way, and
/// every listing rebuilt from it afterwards would be silently wrong. A whole
/// listing has no such dependency: whatever was superseded, the survivor is
/// still the truth, and `Message::superseded` tells the receiver how much it
/// skipped.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "snapshot", content = "of", rename_all = "snake_case")]
pub enum Snapshot {
    Containers(Vec<ContainerSummary>),
    Executions(ExecutionList),
    Images(Vec<ImageSummary>),
    ImagePulls(ImagePullChange),
    Volumes(Vec<VolumeSummary>),
    Networks(Vec<NetworkSummary>),
    Terminal(Vec<TabSummary>),
    PaneChanges(PaneChange),
    Extensions(Vec<ExtensionSummary>),
    ExtensionAcquisitions(ExtensionAcquisitionChange),
    WorkspaceLifecycle(WorkspaceLifecycleChange),
    WorkspaceEvents(WorkspaceEventBatch),
}

impl Snapshot {
    /// The topic this listing belongs to. Routing reads this rather than being
    /// told separately, so a listing cannot be delivered on another topic's
    /// channel.
    #[must_use]
    pub const fn topic(&self) -> Topic {
        match self {
            Self::Containers(_) => Topic::Containers,
            Self::Executions(_) => Topic::Executions,
            Self::Images(_) => Topic::Images,
            Self::ImagePulls(_) => Topic::ImagePulls,
            Self::Volumes(_) => Topic::Volumes,
            Self::Networks(_) => Topic::Networks,
            Self::Terminal(_) => Topic::Terminal,
            Self::PaneChanges(_) => Topic::PaneChanges,
            Self::Extensions(_) => Topic::Extensions,
            Self::ExtensionAcquisitions(_) => Topic::ExtensionAcquisitions,
            Self::WorkspaceLifecycle(_) => Topic::WorkspaceLifecycle,
            Self::WorkspaceEvents(_) => Topic::WorkspaceEvents,
        }
    }

    /// Encodes the listing as a frame payload.
    ///
    /// # Errors
    /// Returns `Coding::Oversize` when the encoded listing exceeds the payload
    /// limit, and `Coding::Malformed` when it cannot be serialized.
    pub fn payload(&self) -> Result<Vec<u8>, Coding> {
        let bytes = serde_json::to_vec(self).map_err(|error| Coding::Malformed(error.to_string()))?;
        if bytes.len() > Frame::PAYLOAD_LIMIT {
            return Err(Coding::Oversize(bytes.len()));
        }
        Ok(bytes)
    }

    /// Applies transport coalescing immediately before delivery. Keeping this
    /// in the bounded metadata means overload is visible without exposing the
    /// generic RPC envelope to domain consumers.
    #[must_use]
    pub fn with_coalesced(mut self, count: u64) -> Self {
        match &mut self {
            Self::PaneChanges(change) => change.coalesced = count,
            Self::ExtensionAcquisitions(change) => change.coalesced = count,
            Self::WorkspaceLifecycle(change) => change.coalesced = change.coalesced.saturating_add(count),
            _ => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExtensionAcquisitionChange, PaneChange, PaneChangeKind, Snapshot, WorkspaceLifecycleAction,
        WorkspaceLifecycleChange,
    };
    use crate::request::Topic;

    #[test]
    fn a_listing_names_the_topic_it_belongs_to() {
        assert_eq!(Snapshot::Containers(Vec::new()).topic(), Topic::Containers);
        assert_eq!(Snapshot::Images(Vec::new()).topic(), Topic::Images);
        assert_eq!(Snapshot::Volumes(Vec::new()).topic(), Topic::Volumes);
        assert_eq!(Snapshot::Networks(Vec::new()).topic(), Topic::Networks);
        assert_eq!(Snapshot::Terminal(Vec::new()).topic(), Topic::Terminal);
        assert_eq!(Snapshot::Extensions(Vec::new()).topic(), Topic::Extensions);
        let acquisition = Snapshot::ExtensionAcquisitions(ExtensionAcquisitionChange {
            job: "job-1".into(),
            revision: 5,
            state: "ready".into(),
            coalesced: 0,
        });
        assert_eq!(acquisition.topic(), Topic::ExtensionAcquisitions);
        assert_eq!(
            acquisition.with_coalesced(8),
            Snapshot::ExtensionAcquisitions(ExtensionAcquisitionChange {
                job: "job-1".into(),
                revision: 5,
                state: "ready".into(),
                coalesced: 8,
            })
        );
        let change = Snapshot::PaneChanges(PaneChange {
            slot: "s1".into(),
            kind: PaneChangeKind::Surface,
            revision: 4,
            generation: 9,
            coalesced: 0,
        });
        assert_eq!(change.topic(), Topic::PaneChanges);
        assert_eq!(
            change.with_coalesced(17),
            Snapshot::PaneChanges(PaneChange {
                slot: "s1".into(),
                kind: PaneChangeKind::Surface,
                revision: 4,
                generation: 9,
                coalesced: 17,
            })
        );
        let lifecycle = Snapshot::WorkspaceLifecycle(WorkspaceLifecycleChange {
            workspace: "dev".into(),
            action: WorkspaceLifecycleAction::Update,
            revision: 12,
            coalesced: 0,
        });
        assert_eq!(lifecycle.topic(), Topic::WorkspaceLifecycle);
        assert!(
            matches!(lifecycle.with_coalesced(3), Snapshot::WorkspaceLifecycle(change) if change.revision == 12 && change.coalesced == 3)
        );
    }
}
