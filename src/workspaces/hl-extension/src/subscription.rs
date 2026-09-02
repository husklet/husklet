//! The listings a workspace pushes to a subscriber.
//!
//! Routing, credit, and the bulk byte streams are `hl-rpc`'s; what is in a
//! listing is this domain's, and is here.

use hl_rpc::{Coding, Frame};

use crate::port::{ContainerSummary, ImageSummary, NetworkSummary, TabSummary, VolumeSummary};
use crate::request::Topic;

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
        x: f64,
        y: f64,
        button: Option<u32>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerPhase {
    Move,
    Enter,
    Leave,
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
    Images(Vec<ImageSummary>),
    Volumes(Vec<VolumeSummary>),
    Networks(Vec<NetworkSummary>),
    Terminal(Vec<TabSummary>),
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
            Self::Images(_) => Topic::Images,
            Self::Volumes(_) => Topic::Volumes,
            Self::Networks(_) => Topic::Networks,
            Self::Terminal(_) => Topic::Terminal,
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
}

#[cfg(test)]
mod tests {
    use super::Snapshot;
    use crate::request::Topic;

    #[test]
    fn a_listing_names_the_topic_it_belongs_to() {
        assert_eq!(Snapshot::Containers(Vec::new()).topic(), Topic::Containers);
        assert_eq!(Snapshot::Images(Vec::new()).topic(), Topic::Images);
        assert_eq!(Snapshot::Volumes(Vec::new()).topic(), Topic::Volumes);
        assert_eq!(Snapshot::Networks(Vec::new()).topic(), Topic::Networks);
        assert_eq!(Snapshot::Terminal(Vec::new()).topic(), Topic::Terminal);
    }
}
