use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hl_container::Error as ContainerError;

use crate::api::DockerError;

pub(super) type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug)]
pub(super) struct ApiError {
    pub(super) status: StatusCode,
    message: String,
}

impl ApiError {
    pub(super) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(super) fn image(error: hl_images::Error) -> Self {
        let message = error.to_string();
        drop(error);
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub(super) fn image_request(error: hl_images::Error) -> Self {
        let message = error.to_string();
        let status = match &error {
            hl_images::Error::InvalidReference(_)
            | hl_images::Error::InvalidDigest(_)
            | hl_images::Error::DigestMismatch { .. }
            | hl_images::Error::SizeMismatch { .. }
            | hl_images::Error::UnsafeArchive { .. }
            | hl_images::Error::MalformedOci(_)
            | hl_images::Error::DiffIdMismatch { .. }
            | hl_images::Error::Json(_) => StatusCode::BAD_REQUEST,
            hl_images::Error::ContentNotFound(_)
            | hl_images::Error::NotOwned { .. }
            | hl_images::Error::InvalidMetadata(_)
            | hl_images::Error::Registry(_)
            | hl_images::Error::UnsupportedPlatform { .. }
            | hl_images::Error::LayerFilesystem { .. }
            | hl_images::Error::Io { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        drop(error);
        Self::new(status, message)
    }

    pub(super) fn task(error: tokio::task::JoinError) -> Self {
        let message = error.to_string();
        drop(error);
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub(super) fn io(error: std::io::Error) -> Self {
        let message = error.to_string();
        drop(error);
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    #[allow(clippy::needless_pass_by_value)] // `Result::map_err` supplies the owned error.
    pub(super) fn container(error: ContainerError) -> Self {
        let message = error.to_string();
        let status = match error {
            ContainerError::NotFound(_)
            | ContainerError::ExecNotFound(_)
            | ContainerError::VolumeNotFound(_)
            | ContainerError::NetworkNotFound(_) => StatusCode::NOT_FOUND,
            ContainerError::NameConflict(_)
            | ContainerError::PortConflict(_, _)
            | ContainerError::VolumeConflict(_)
            | ContainerError::VolumeInUse(_)
            | ContainerError::NetworkConflict(_)
            | ContainerError::NetworkInUse(_)
            | ContainerError::AlreadyConnected { .. }
            | ContainerError::NotConnected { .. }
            | ContainerError::InvalidState { .. }
            | ContainerError::InvalidExecState { .. }
            | ContainerError::NoTerminal(_)
            | ContainerError::AlreadyRunning(_) => StatusCode::CONFLICT,
            ContainerError::InvalidSpec(_) | ContainerError::InvalidVolume(_) | ContainerError::InvalidNetwork(_) => {
                StatusCode::BAD_REQUEST
            }
            ContainerError::ReadOnly(_) => StatusCode::FORBIDDEN,
            // A named capability gap, not a client mistake and not a runtime fault: the restored
            // member exists, and nothing here can hand the caller a handle to it yet.
            ContainerError::ExecNotReattachable { .. } => StatusCode::NOT_IMPLEMENTED,
            ContainerError::Io(ref error) if error.kind() == std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ContainerError::Runtime(_)
            | ContainerError::StopTimeout { .. }
            | ContainerError::Corrupt(_)
            | ContainerError::TranslationCache(_)
            | ContainerError::Io(_)
            | ContainerError::Json(_)
            | ContainerError::Image(_)
            | ContainerError::Checkpoint(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(DockerError { message: self.message })).into_response()
    }
}
