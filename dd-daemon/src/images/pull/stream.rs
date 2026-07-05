//! Streamed `docker pull` progress plumbing: newline-delimited JSON status lines, the live
//! [`PullEvent`] → docker-shaped bar formatter, the synthetic-layer progress sequence, and the
//! `X-Registry-Auth` decoder.
use super::*;
use crate::registry::{Credentials, PullEvent};
use serde_json::json;

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
        emit!(json!({ "status": format!("Pulling from {repo}"), "id": tag }));
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
                emit!(json!({ "status": format!("Digest: {digest}") }));
                emit!(
                    json!({ "status": format!("Status: Downloaded newer image for {name}:{tag}") })
                );
            }
            Err(e) => emit!(json!({ "errorDetail": { "message": e.clone() }, "error": e })),
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
        PullEvent::Layer { id } => json!({ "status": "Pulling fs layer", "id": id }),
        PullEvent::Downloading { id, current, total } => {
            json!({ "status": "Downloading", "progressDetail": { "current": current, "total": total }, "id": id })
        }
        PullEvent::DownloadComplete { id } => json!({ "status": "Download complete", "id": id }),
        PullEvent::Extracting { id, current, total } => {
            json!({ "status": "Extracting", "progressDetail": { "current": current, "total": total }, "id": id })
        }
        PullEvent::PullComplete { id } => json!({ "status": "Pull complete", "id": id }),
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
/// they drive a docker-shaped per-layer progress sequence on a fresh pull. dd squashes an image to a
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
            json!({ "status": format!("Status: Image is up to date for {name}:{tag}") })
        ),
        Ok(false) => {
            let repo = image_ref(name, tag).repository;
            let layer_id = digest
                .trim_start_matches("sha256:")
                .chars()
                .take(12)
                .collect::<String>();
            let layer = layer_id.as_str();
            let half = (size / 2).max(0);
            [
                json!({ "status": format!("Pulling from {repo}"), "id": tag }).to_string(),
                json!({ "status": "Pulling fs layer", "id": layer }).to_string(),
                json!({ "status": "Downloading", "progressDetail": { "current": half, "total": size }, "id": layer }).to_string(),
                json!({ "status": "Downloading", "progressDetail": { "current": size, "total": size }, "id": layer }).to_string(),
                json!({ "status": "Verifying Checksum", "id": layer }).to_string(),
                json!({ "status": "Download complete", "id": layer }).to_string(),
                json!({ "status": "Extracting", "progressDetail": { "current": size, "total": size }, "id": layer }).to_string(),
                json!({ "status": "Pull complete", "id": layer }).to_string(),
                json!({ "status": format!("Digest: {digest}") }).to_string(),
                json!({ "status": format!("Status: Downloaded newer image for {name}:{tag}") }).to_string(),
            ].join("\r\n") + "\r\n"
        }
        Err(e) => {
            json!({ "errorDetail": { "message": e.clone() }, "error": e }).to_string() + "\r\n"
        }
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}
