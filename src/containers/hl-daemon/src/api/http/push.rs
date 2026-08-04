use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::Response,
};
use hl_images::{
    Reference,
    remote::{Auth, Registry},
};
use serde::Deserialize;

use super::DockerState;
use crate::api::{Credentials, DockerError, PushAux, PushProgress};

#[derive(Default, Deserialize)]
pub(super) struct Options {
    tag: Option<String>,
}

pub(super) async fn post(
    State(state): State<DockerState>,
    Path(name): Path<String>,
    Query(options): Query<Options>,
    headers: HeaderMap,
) -> Response {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(2);
    tokio::spawn(async move {
        let result = async {
            let requested = options
                .tag
                .filter(|tag| !tag.is_empty())
                .map_or_else(|| name.clone(), |tag| format!("{name}:{tag}"));
            let target: Reference = requested.parse()?;
            let image = state
                .find_image(&requested)
                .await
                .map_err(|_| hl_images::Error::ContentNotFound(requested.clone()))?;
            let images = state
                .containers
                .images()
                .map_err(|error| hl_images::Error::InvalidMetadata(error.to_string()))?;
            let size = images.size(&image)?;
            let auth = Credentials::from_headers(&headers)?;
            let _ = sender
                .send(Ok(PushProgress {
                    status: Some(format!("The push refers to repository [{}]", target.repository())),
                    id: Some(image.target.digest().to_string()),
                    ..PushProgress::default()
                }
                .bytes()))
                .await;
            Registry::new(auth).push(&image, &target, images.content()).await?;
            Ok::<_, hl_images::Error>((target, image, size))
        }
        .await;
        let progress = result.map_or_else(|error| PushProgress::from_error(&error), PushProgress::from_push);
        let _ = sender.send(Ok(progress.bytes())).await;
    });
    Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(futures_util::stream::unfold(
            receiver,
            |mut receiver| async { receiver.recv().await.map(|item| (item, receiver)) },
        )))
        .expect("push response is valid")
}

impl PushProgress {
    fn from_push((target, image, size): (Reference, hl_images::Image, u64)) -> Self {
        Self {
            status: Some("Pushed".into()),
            id: Some(image.target.digest().to_string()),
            aux: Some(PushAux {
                tag: target.tag().unwrap_or("latest").into(),
                digest: image.target.digest().to_string(),
                size: i64::try_from(size).unwrap_or(i64::MAX),
            }),
            ..Self::default()
        }
    }

    fn from_error(error: &hl_images::Error) -> Self {
        let message = error.to_string();
        Self {
            error: Some(message.clone()),
            error_detail: Some(DockerError { message }),
            ..Self::default()
        }
    }
}

impl Credentials {
    fn from_headers(headers: &HeaderMap) -> hl_images::Result<Auth> {
        let Some(value) = headers.get("x-registry-auth") else {
            return Ok(Auth::Anonymous);
        };
        let value = value
            .to_str()
            .map_err(|_| hl_images::Error::InvalidMetadata("invalid X-Registry-Auth header".into()))?;
        Self::decode(value)
            .and_then(Self::auth)
            .map_err(|error| hl_images::Error::InvalidMetadata(format!("invalid X-Registry-Auth header: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn header(value: &serde_json::Value) -> HeaderMap {
        let encoded = base64::engine::general_purpose::STANDARD.encode(value.to_string());
        let mut headers = HeaderMap::new();
        headers.insert("x-registry-auth", encoded.parse().unwrap());
        headers
    }

    #[test]
    fn decodes_basic_and_bearer_docker_credentials() {
        assert!(matches!(
            Credentials::from_headers(&header(&serde_json::json!({
                "username": "user",
                "password": "secret",
                "serveraddress": "registry.example.test"
            })))
            .unwrap(),
            Auth::Basic { username, password } if username == "user" && password == "secret"
        ));
        assert!(matches!(
            Credentials::from_headers(&header(&serde_json::json!({"identitytoken": "token"})))
                .unwrap(),
            Auth::Bearer(token) if token == "token"
        ));
    }
}
