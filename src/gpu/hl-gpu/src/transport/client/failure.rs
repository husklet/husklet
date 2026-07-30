use super::*;
use std::io::ErrorKind;

impl RemoteCommandSink {
    pub fn terminal_error(&self) -> Option<&TransportError> {
        self.terminal.as_ref()
    }

    /// Open and negotiate before a sandbox prevents opening the projected socket.
    pub fn connect(&mut self) -> Result<()> {
        self.ensure()
    }

    pub(super) fn diagnostics(&self) -> bool {
        self.trace
            || (hl_log::VERBOSE_COMPILED
                && hl_log::Logging::global().enabled(
                    hl_log::Tags::from(hl_log::tag::TRANSPORT),
                    hl_log::Level::Debug,
                ))
    }

    pub(super) fn timeout(error: &io::Error) -> bool {
        matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
    }

    pub(super) fn unavailable(phase: TransportPhase, error: io::Error) -> TransportError {
        TransportError::Unavailable {
            phase,
            detail: error.to_string(),
        }
    }

    pub(super) fn before_request(phase: TransportPhase, error: io::Error) -> TransportError {
        if Self::timeout(&error) {
            TransportError::Timeout {
                phase,
                ambiguous: false,
            }
        } else {
            Self::unavailable(phase, error)
        }
    }

    pub(super) fn ambiguous(&mut self, phase: TransportPhase, error: io::Error) -> GpuError {
        let failure = if Self::timeout(&error) {
            TransportError::Timeout {
                phase,
                ambiguous: true,
            }
        } else {
            TransportError::Ambiguous {
                phase,
                detail: error.to_string(),
            }
        };
        self.lose(failure)
    }

    pub(super) fn protocol(&mut self, phase: TransportPhase, detail: &str) -> GpuError {
        self.lose(TransportError::Ambiguous {
            phase,
            detail: detail.into(),
        })
    }

    pub(super) fn lose_api(&mut self, detail: &str) -> GpuError {
        self.lose(TransportError::ApiLost {
            detail: detail.into(),
        })
    }

    pub(super) fn lose(&mut self, failure: TransportError) -> GpuError {
        self.sock = None;
        self.terminal = Some(failure.clone());
        GpuError::Transport(failure)
    }

    pub(super) fn request(
        &mut self,
        request: &ReadbackRequest,
        expected: usize,
        response_timeout: std::time::Duration,
    ) -> Result<Vec<u8>> {
        let mut last_error = None;
        for _ in 0..2 {
            self.ensure()?;
            if self.residency_reset {
                self.submit_ir(&[], &[], 0)?;
            }
            let peer_closed = {
                let socket = self
                    .sock
                    .as_ref()
                    .expect("residency restore installed socket");
                unix::Connection::new(socket).peer_closed()
            };
            match peer_closed {
                Ok(true) => {
                    self.sock = None;
                    last_error = Some(GpuError::Transport(TransportError::Unavailable {
                        phase: TransportPhase::RequestWrite,
                        detail: "peer closed before request".into(),
                    }));
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    self.sock = None;
                    last_error = Some(GpuError::Transport(Self::before_request(
                        TransportPhase::RequestWrite,
                        error,
                    )));
                    continue;
                }
            }
            {
                let socket = self
                    .sock
                    .as_ref()
                    .expect("request socket remains installed");
                socket
                    .set_read_timeout(Some(response_timeout))
                    .map_err(|error| {
                        GpuError::Transport(Self::unavailable(TransportPhase::ResponseRead, error))
                    })?;
            }
            let write = {
                let socket = self
                    .sock
                    .as_ref()
                    .expect("request socket remains installed");
                unix::Connection::new(socket).write_readback_request_tracked(request)
            };
            if let Err(failure) = write {
                if failure.accepted != 0 {
                    return Err(self.ambiguous(TransportPhase::RequestWrite, failure.error));
                }
                self.sock = None;
                last_error = Some(GpuError::Transport(Self::before_request(
                    TransportPhase::RequestWrite,
                    failure.error,
                )));
                continue;
            }
            let response = {
                let socket = self
                    .sock
                    .as_ref()
                    .expect("request socket remains installed");
                unix::Connection::new(socket).read_readback_response(expected)
            };
            let restored = self
                .sock
                .as_ref()
                .expect("request socket remains installed")
                .set_read_timeout(Some(self.config.response_timeout()));
            match response {
                Ok(bytes) => {
                    if restored.is_err() {
                        self.sock = None;
                    }
                    return Ok(bytes);
                }
                Err(unix::ReadbackResponseError::Rejected) => {
                    if restored.is_err() {
                        self.sock = None;
                    }
                    return Err(GpuError::Transport(TransportError::Rejected {
                        phase: TransportPhase::ResponseRead,
                        acknowledgement: crate::transport::model::readback::READBACK_FAIL,
                    }));
                }
                Err(unix::ReadbackResponseError::Malformed(detail)) => {
                    return Err(self.protocol(TransportPhase::ResponseRead, &detail));
                }
                Err(unix::ReadbackResponseError::Io(error)) => {
                    return Err(self.ambiguous(TransportPhase::ResponseRead, error));
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            GpuError::Transport(TransportError::Unavailable {
                phase: TransportPhase::RequestWrite,
                detail: "request attempts exhausted".into(),
            })
        }))
    }
}
