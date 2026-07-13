//! `docker import` -- extract a bare rootfs tar (no manifest) into a new image and register it.
use super::*;

/// `docker import` -- extract a bare rootfs tar (request body) into a new image named by `repo`
/// (optionally `repo:tag`) and register it. Routed from `images_create` on `fromSrc`.
pub(crate) async fn image_import(
    a: App,
    repo: String,
    tag: String,
    src: &str,
    body: axum::body::Bytes,
) -> Response {
    if repo.is_empty() {
        return import_progress(Err("repo is required".into()));
    }
    // dd imports a rootfs tar streamed in the body (`docker import - <name>`); importing from a remote
    // URL is not supported (dd has no HTTP fetcher).
    if src != "-" {
        return import_progress(Err(format!(
            "unsupported import source {src:?}; pipe the rootfs to `-`"
        )));
    }
    let name = if tag.is_empty() {
        repo
    } else {
        format!("{repo}:{tag}")
    };
    // dd-images owns the rootfs-tar extraction into a new image dir (+ minimal `dd-image.json` sidecar);
    // the handler maps the result onto the daemon's `Image` and registers it.
    let loaded = match hl_images::Store::new(&a.images_dir)
        .import_rootfs(&name, &body)
        .map_err(|e| e.to_string())
    {
        Ok(l) => l,
        Err(e) => return import_progress(Err(e)),
    };
    let img = Image {
        name: loaded.name,
        rootfs: loaded.rootfs.to_string_lossy().into_owned(),
        arch: guest_of(loaded.arch),
        cmd: loaded.cmd,
        created: now_secs(),
        ..Default::default()
    };
    register_image(&a, img).await;
    import_progress(Ok(format!("sha256:{}", fake_id(&name))))
}

/// `docker import` progress: a single JSON status line carrying the new image id, or an error line.
fn import_progress(result: Result<String, String>) -> Response {
    let body = match result {
        Ok(id) => serde_json::to_string(&ImportStatus { status: id }).unwrap() + "\r\n",
        Err(e) => {
            json!({ "errorDetail": { "message": e.clone() }, "error": e }).to_string() + "\r\n"
        }
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}
