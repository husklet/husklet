//! Finalized-versus-prepared classification for a checkpoint generation.
//!
//! The checkpoint byte store is adversarial and its committed-generation
//! pointer is data, not authority. Recovery therefore has to decide, locally
//! and before any object is served, whether the generation it was offered was
//! finalized or merely staged. That decision lives here.

/// Object name written by the storage transaction's irrevocable commit step.
/// Its presence in a generation is the only local evidence that the generation
/// was finalized rather than merely staged.
pub(super) const MANIFEST_OBJECT: &str = "MANIFEST";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordState {
    Prepared,
    Finalized,
}

impl RecordState {
    /// Classifies a checkpoint generation from the object names the byte store
    /// exposes. A staged generation whose transaction never reached `commit`
    /// carries objects but no manifest, and so is `Prepared`. An empty
    /// generation is `Prepared` for the same reason -- nothing proves it was
    /// finalized.
    pub(super) fn of_generation<S: AsRef<str>>(names: &[S]) -> Self {
        if names.iter().any(|name| name.as_ref() == MANIFEST_OBJECT) {
            Self::Finalized
        } else {
            Self::Prepared
        }
    }

    /// Recovery may read a generation only after it is finalized. A `Prepared`
    /// record was never committed, so it may not be handed to native restore.
    pub(super) fn admits_recovery(self) -> bool {
        matches!(self, Self::Finalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_manifest_bearing_generation_admits_recovery() {
        assert_eq!(RecordState::of_generation::<&str>(&[]), RecordState::Prepared);
        assert_eq!(
            RecordState::of_generation(&["proc.1/pages", "proc.1/arena"]),
            RecordState::Prepared
        );
        assert_eq!(
            RecordState::of_generation(&["proc.1/pages", MANIFEST_OBJECT]),
            RecordState::Finalized
        );
        assert!(!RecordState::Prepared.admits_recovery());
        assert!(RecordState::Finalized.admits_recovery());
    }
}
