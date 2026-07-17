//! Streamed `docker pull` progress plumbing: newline-delimited JSON status lines, the live
//! [`PullEvent`] → docker-shaped bar formatter, the synthetic-layer progress sequence, and the
//! `X-Registry-Auth` decoder.
use super::*;
use crate::api::{ErrorMessage, ProgressDetail, PullError, PullProgress};
use crate::registry::{Credentials, PullEvent};
use serde_json::json;

/// One `docker pull` status line as a docker-shaped JSON value, routed through the typed
/// [`PullProgress`] DTO. Going through `to_value` (rather than serializing the struct directly) keeps
/// the emitted bytes byte-identical to the old inline `json!` — both produce a `serde_json::Value`
/// whose `to_string` renders keys in the same canonical order.
fn pull_line(status: String, id: Option<String>, detail: Option<ProgressDetail>) -> Value {
    serde_json::to_value(PullProgress {
        status,
        progress_detail: detail,
        id,
    })
    .unwrap()
}

/// The pull stream's error line as a JSON value, via the typed [`PullError`] DTO.
fn pull_error_line(e: String) -> Value {
    serde_json::to_value(PullError {
        error_detail: ErrorMessage { message: e.clone() },
        error: e,
    })
    .unwrap()
}

/// Stream a fresh `docker pull` as newline-delimited JSON, flushing each status line as the download
/// proceeds (mirrors the `events.rs` streamed-body pattern). A background task drives the blocking
/// registry pull, forwarding its per-layer [`PullEvent`]s into the response body live; on completion it
/// registers the image and emits the closing `Digest:`/`Status:` lines (or an error line). This replaces
/// the old "block until done, then dump a fixed sequence" behavior so the client renders moving bars.
pub(super) fn pull_stream(
    a: App,
    name: String,
    tag: String,
    want: String,
    creds: Credentials,
    archs: Vec<&'static str>,
) -> Response {
    // Lines flow out through `line_rx`; the body stream just drains it (closed when the worker drops tx).
    // An awaited `send` gives natural backpressure (a slow/stalled client throttles the producer rather
    // than silently dropping lines); a send error just means the client hung up — stop quietly.
    let (line_tx, line_rx) = mpsc::channel::<Vec<u8>>(256);
    tokio::spawn(async move {
        macro_rules! emit {
            ($v:expr) => {
                if line_tx
                    .send(($v.to_string() + "\r\n").into_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            };
        }
        let repo = image_ref(&name, &tag).repository;
        emit!(pull_line(
            format!("Pulling from {repo}"),
            Some(tag.clone()),
            None
        ));
        // The blocking pull reports progress over `pev`; forward+format each event into a status line.
        let (pev_tx, mut pev_rx) = mpsc::channel::<PullEvent>(256);
        let (dir, nm, tg) = (a.images_dir.clone(), name.clone(), tag.clone());
        let blocking = tokio::task::spawn_blocking(move || {
            let mut cb = |e: PullEvent| {
                let _ = pev_tx.blocking_send(e);
            };
            pull_image(&dir, &nm, &tg, creds, &archs, &mut cb)
        });
        while let Some(e) = pev_rx.recv().await {
            emit!(pull_event_json(&e));
        }
        let res = blocking
            .await
            .unwrap_or_else(|e| Err(format!("pull task crashed: {e}")));
        match res {
            Ok(img) => {
                let digest = format!("sha256:{}", fake_id(&img.name));
                {
                    let mut g = a.inner.lock().await;
                    g.images.retain(|i| repo_tag(&i.name) != want); // a re-pull (new platform) replaces the old
                    g.images.push(img);
                }
                crate::events::emit_event(&a.events, "image", "pull", &want, json!({"name": want}));
                emit!(pull_line(format!("Digest: {digest}"), None, None));
                emit!(pull_line(
                    format!("Status: Downloaded newer image for {name}:{tag}"),
                    None,
                    None
                ));
            }
            Err(e) => emit!(pull_error_line(e)),
        }
    });
    let body = futures_util::stream::unfold(line_rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|b| (Ok::<Vec<u8>, std::io::Error>(b), rx))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from_stream(body))
        .unwrap()
}

/// Format one live [`PullEvent`] into the docker-shaped JSON status object the CLI renders as a bar.
fn pull_event_json(e: &PullEvent) -> Value {
    match e {
        PullEvent::Layer { id } => pull_line("Pulling fs layer".into(), Some(id.clone()), None),
        PullEvent::Downloading { id, current, total } => pull_line(
            "Downloading".into(),
            Some(id.clone()),
            Some(ProgressDetail {
                current: *current as i64,
                total: *total as i64,
            }),
        ),
        PullEvent::DownloadComplete { id } => {
            pull_line("Download complete".into(), Some(id.clone()), None)
        }
        PullEvent::Extracting { id, current, total } => pull_line(
            "Extracting".into(),
            Some(id.clone()),
            Some(ProgressDetail {
                current: *current as i64,
                total: *total as i64,
            }),
        ),
        PullEvent::PullComplete { id } => pull_line("Pull complete".into(), Some(id.clone()), None),
    }
}

/// Decode the CLI's `X-Registry-Auth` header (base64 JSON credentials) into [`Credentials`].
pub(crate) fn registry_auth(headers: &axum::http::HeaderMap) -> Credentials {
    headers
        .get("X-Registry-Auth")
        .and_then(|v| v.to_str().ok())
        .and_then(Credentials::from_x_registry_auth)
        .unwrap_or_default()
}

/// docker-style pull progress: a newline-delimited stream of JSON status lines the CLI renders.
///
/// `digest`/`size` describe the pulled image (its synthetic content digest and on-disk rootfs size);
/// they drive a docker-shaped per-layer progress sequence on a fresh pull. hl squashes an image to a
/// single rootfs, so we surface ONE synthetic layer (id = first 12 hex of the digest) rather than the
/// registry's real per-blob layers. See the registry note in the push helper for what real byte
/// progress would require.
pub(crate) fn pull_progress(
    name: &str,
    tag: &str,
    result: Result<bool, String>,
    digest: &str,
    size: i64,
) -> Response {
    let body = match result {
        Ok(true) => format!(
            "{}\r\n",
            pull_line(
                format!("Status: Image is up to date for {name}:{tag}"),
                None,
                None
            )
        ),
        Ok(false) => {
            let repo = image_ref(name, tag).repository;
            let layer_id = layer_short(&digest);
            let layer = layer_id.as_str();
            let half = (size / 2).max(0);
            let id = || Some(layer.to_string());
            [
                pull_line(format!("Pulling from {repo}"), Some(tag.to_string()), None).to_string(),
                pull_line("Pulling fs layer".into(), id(), None).to_string(),
                pull_line(
                    "Downloading".into(),
                    id(),
                    Some(ProgressDetail {
                        current: half,
                        total: size,
                    }),
                )
                .to_string(),
                pull_line(
                    "Downloading".into(),
                    id(),
                    Some(ProgressDetail {
                        current: size,
                        total: size,
                    }),
                )
                .to_string(),
                pull_line("Verifying Checksum".into(), id(), None).to_string(),
                pull_line("Download complete".into(), id(), None).to_string(),
                pull_line(
                    "Extracting".into(),
                    id(),
                    Some(ProgressDetail {
                        current: size,
                        total: size,
                    }),
                )
                .to_string(),
                pull_line("Pull complete".into(), id(), None).to_string(),
                pull_line(format!("Digest: {digest}"), None, None).to_string(),
                pull_line(
                    format!("Status: Downloaded newer image for {name}:{tag}"),
                    None,
                    None,
                )
                .to_string(),
            ]
            .join("\r\n")
                + "\r\n"
        }
        Err(e) => pull_error_line(e).to_string() + "\r\n",
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}
