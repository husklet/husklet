//! `docker push` -- re-tar the local rootfs into a single-layer image and upload it to its registry.
use super::*;
use crate::registry::Client;

/// POST /images/:name/push -- re-tar the local rootfs into a single-layer image and upload it to its
/// registry (`docker.io/...`, `ghcr.io/...`, `localhost:5000/...`) using the CLI's credentials.
pub(crate) async fn image_push(
    State(a): State<App>,
    Path(name): Path<String>,
    Query(q): Query<PushQ>,
    headers: axum::http::HeaderMap,
) -> Response {
    // The route `name` is collapsed to the bare image (e.g. `huttarichard/ddmac` -> `ddmac`), so match on
    // it AND the requested tag, then push to the image's FULL stored name so the registry namespace
    // (`huttarichard/…`) is preserved — otherwise the upload targets `library/<name>` and is denied.
    let want_tag = q
        .tag
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "latest".into());
    let img = {
        let g = a.inner.lock().await;
        g.images
            .iter()
            .find(|i| ref_name(&i.name) == ref_name(&name) && ref_tag(&i.name) == want_tag)
            .or_else(|| {
                g.images
                    .iter()
                    .find(|i| ref_name(&i.name) == ref_name(&name))
            })
            .cloned()
    };
    let Some(img) = img else {
        return push_progress(&name, &want_tag, 0, Err(format!("No such image: {name}")))
            .into_response();
    };
    let tag = want_tag;
    let iref = image_ref(&img.name, &tag);
    let arch = docker_arch(img.arch).to_string();
    let os = img.arch.os().to_string(); // "darwin" for mac images, else "linux"
    let creds = registry_auth(&headers);
    // On-disk rootfs size, captured before `img` is moved into the push task; reported as the layer
    // `Size` in the push progress/aux lines (a real registry manifest size would need registry.rs to
    // surface it — see note below).
    let size = image_size(&img.rootfs, &img.name);
    // Unique per request: a bare `.push-<pid>` collides when two pushes run concurrently in one daemon
    // process (see `next_staging_seq`). (Only the staging PATH is per-request; the push payload/config
    // serialization is unchanged.)
    let work = std::path::PathBuf::from(format!(
        "{}/.push-{}-{}",
        a.images_dir,
        std::process::id(),
        crate::util::next_staging_seq()
    ));
    let res = tokio::task::spawn_blocking(move || {
        Client::new(iref, creds)
            .push(
                std::path::Path::new(&img.rootfs),
                &img.cmd,
                &arch,
                &os,
                &work,
            )
            .map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("push task crashed: {e}")));
    push_progress(&name, &tag, size, res).into_response()
}

#[derive(Deserialize)]
pub(crate) struct PushQ {
    tag: Option<String>,
}

/// docker-style push progress: a newline-delimited stream of JSON status lines (or an error line).
///
/// `digest` is the manifest digest returned by `Client::push` (the registry's `Docker-Content-Digest`),
/// `size` is the image's on-disk rootfs size used as the layer/aux `Size`. The stream ends with the
/// `aux` line (which the docker CLI parses to print `digest: … size: …`) followed by the matching
/// status line, so `docker push` reports the real pushed digest instead of a hardcoded `latest:`.
///
/// REPORT-only: a fully accurate `Size` would be the manifest byte length, not the rootfs size. The
/// registry client computes both the layer size and the manifest bytes internally; if `Client::push`
/// returned `(digest, manifest_size, layer_size)` instead of just the digest, dd could emit Docker's
/// exact `size:` value and real per-blob byte progress here.
pub(crate) fn push_progress(
    name: &str,
    tag: &str,
    size: i64,
    result: Result<String, String>,
) -> Response {
    let body = match result {
        Ok(digest) => {
            let layer_id = layer_short(&digest);
            let half = (size / 2).max(0);
            let status = |s: String| StreamStatus {
                status: s,
                progress_detail: None,
                id: None,
            };
            let pushing = |current: i64| StreamStatus {
                status: "Pushing".into(),
                progress_detail: Some(ProgressDetail { current, total: size }),
                id: Some(layer_id.clone()),
            };
            [
                serde_json::to_string(&status(format!("The push refers to repository [{name}]")))
                    .unwrap(),
                serde_json::to_string(&StreamStatus {
                    status: "Preparing".into(),
                    progress_detail: None,
                    id: Some(layer_id.clone()),
                })
                .unwrap(),
                serde_json::to_string(&pushing(half)).unwrap(),
                serde_json::to_string(&pushing(size)).unwrap(),
                serde_json::to_string(&StreamStatus {
                    status: "Pushed".into(),
                    progress_detail: None,
                    id: Some(layer_id.clone()),
                })
                .unwrap(),
                serde_json::to_string(&AuxLine {
                    progress_detail: Empty {},
                    aux: Aux {
                        tag: tag.to_string(),
                        digest: digest.clone(),
                        size,
                    },
                })
                .unwrap(),
                serde_json::to_string(&status(format!("{tag}: digest: {digest} size: {size}")))
                    .unwrap(),
            ]
            .join("\r\n")
                + "\r\n"
        }
        Err(e) => {
            json!({ "errorDetail": { "message": e.clone() }, "error": e }).to_string() + "\r\n"
        }
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}
