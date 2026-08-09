use axum::http::StatusCode;
use hl_container::{ExecId, Executions, Signal};
use serde::Deserialize;

use super::error::{ApiError, ApiResult};

mod connection;
mod detach;

pub(super) use connection::Connection;
pub(super) use detach::DetachKeys;

#[derive(Deserialize)]
pub(super) struct Resize {
    #[serde(rename = "h")]
    height: u64,
    #[serde(rename = "w")]
    width: u64,
}

impl Resize {
    pub(super) fn size(self) -> ApiResult<hl_container::Size> {
        let rows = u16::try_from(self.height)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "terminal height exceeds 65535"))?;
        let columns = u16::try_from(self.width)
            .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "terminal width exceeds 65535"))?;
        hl_container::Size::new(rows, columns).map_err(ApiError::container)
    }
}

struct Disconnect {
    executions: Executions,
    id: ExecId,
}

impl Disconnect {
    async fn cleanup(self) {
        if let Err(error) = self.executions.signal(&self.id, Signal::KILL).await {
            hl_log::hl_warn!(
                hl_log::tag::DAEMON,
                "disconnected exec signal failed id={} error={error}",
                self.id
            );
        }
        for _ in 0..500 {
            match self.executions.remove(&self.id).await {
                Ok(()) | Err(hl_container::Error::NotFound(_)) => return,
                Err(hl_container::Error::InvalidExecState { .. }) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => {
                    hl_log::hl_error!(
                        hl_log::tag::DAEMON,
                        "disconnected exec cleanup failed id={} error={error}",
                        self.id
                    );
                    return;
                }
            }
        }
        hl_log::hl_error!(
            hl_log::tag::DAEMON,
            "disconnected exec cleanup timed out id={}",
            self.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_contract() {
        let size = Resize { height: 41, width: 109 }.size().unwrap();
        assert_eq!((size.rows(), size.columns()), (41, 109));
        assert!(
            Resize {
                height: 65_536,
                width: 80,
            }
            .size()
            .is_err()
        );
        assert!(Resize { height: 24, width: 0 }.size().is_err());
    }
}
