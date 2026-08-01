/// The transport phase that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportPhase {
    Connect,
    Handshake,
    FrameWrite,
    Acknowledgement,
    RequestWrite,
    ResponseRead,
}

/// A typed remote transport failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// No request bytes reached the peer, so a caller may retry on a fresh connection.
    Unavailable {
        phase: TransportPhase,
        detail: String,
    },
    /// The configured phase deadline elapsed.
    Timeout {
        phase: TransportPhase,
        /// The peer may already have acted on the request.
        ambiguous: bool,
    },
    /// The peer may have acted on a request whose outcome could not be observed.
    Ambiguous {
        phase: TransportPhase,
        detail: String,
    },
    /// The peer explicitly rejected a complete request.
    Rejected {
        phase: TransportPhase,
        acknowledgement: u8,
    },
    /// The executor contract changed or can no longer reconstruct acknowledged residency.
    ApiLost { detail: String },
    /// An earlier ambiguous failure permanently retired this sink.
    Poisoned { cause: String },
}

impl TransportError {
    pub fn phase(&self) -> Option<TransportPhase> {
        match self {
            Self::Unavailable { phase, .. }
            | Self::Timeout { phase, .. }
            | Self::Ambiguous { phase, .. }
            | Self::Rejected { phase, .. } => Some(*phase),
            Self::ApiLost { .. } | Self::Poisoned { .. } => None,
        }
    }

    /// Whether the peer received a complete request, understood it, and refused it — as opposed to the
    /// connection being gone, ambiguous, or retired.
    ///
    /// The distinction is what a caller needs to decide the BLAST RADIUS of a failure. A refusal is an
    /// ordinary error belonging to the one request that provoked it: the runtime rejects a batch
    /// atomically (see `runtime::submit` — a refused batch leaves the id lifecycle and the residency
    /// ledger exactly as they were), the connection is not retired, and the next request is as likely to
    /// succeed as it was before. Every other kind means the transport itself can no longer be trusted, so
    /// nothing behind it is recoverable.
    pub fn refusal(&self) -> bool {
        match self {
            Self::Rejected { .. } => true,
            Self::Unavailable { .. }
            | Self::Timeout { .. }
            | Self::Ambiguous { .. }
            | Self::ApiLost { .. }
            | Self::Poisoned { .. } => false,
        }
    }

    /// The class of a refusal, as far as the acknowledgement byte states it. `None` when this failure is
    /// not a refusal at all — the connection is gone, ambiguous, or retired, and nothing behind it is
    /// recoverable.
    pub fn refusal_kind(&self) -> Option<crate::transport::model::header::RefusalKind> {
        match self {
            Self::Rejected {
                acknowledgement, ..
            } => Some(crate::transport::model::header::RefusalKind::from_ack(
                *acknowledgement,
            )),
            _ => None,
        }
    }

    pub fn retryable_before_request(&self) -> bool {
        matches!(
            self,
            Self::Unavailable { .. }
                | Self::Timeout {
                    ambiguous: false,
                    ..
                }
        )
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { phase, detail } => {
                write!(f, "transport {phase:?} unavailable: {detail}")
            }
            Self::Timeout { phase, ambiguous } => {
                write!(f, "transport {phase:?} timed out")?;
                if *ambiguous {
                    f.write_str(" with an ambiguous outcome")?;
                }
                Ok(())
            }
            Self::Ambiguous { phase, detail } => {
                write!(f, "transport {phase:?} outcome is ambiguous: {detail}")
            }
            Self::Rejected {
                phase,
                acknowledgement,
            } => {
                write!(
                    f,
                    "host rejected request during {phase:?} (ack={acknowledgement})"
                )
            }
            Self::ApiLost { detail } => write!(f, "GPU API lost: {detail}"),
            Self::Poisoned { cause } => write!(f, "transport is lost: {cause}"),
        }
    }
}

impl std::error::Error for TransportError {}
