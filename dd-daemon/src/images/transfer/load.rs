//! `docker load` -- extract a dd save archive (rootfs/ + dd-manifest.json) into a new image.
use super::*;

/// POST /images/load -- `docker load`. Extracts a dd save archive (rootfs/ + dd-manifest.json) from
/// the request body into a new image directory and registers the image.
pub(crate) async fn image_load(State(a): State<App>, body: axum::body::Bytes) -> Response {
    // dd-images owns the archive extraction + manifest + on-disk placement (`rootfs/` under the store, a
    // `dd-image.json` sidecar so it round-trips through discovery). The handler maps the materialized
    // `LoadedImage` onto the daemon's `Image`, registers it, and emits the load event.
    let loaded = match dd_images::Store::new(&a.images_dir).load_archive(&body) {
        Ok(l) => l,
        Err(e) => return load_err(e),
    };
    let name = loaded.name.clone();
    let healthcheck = loaded
        .healthcheck
        .clone()
        .and_then(|v| serde_json::from_value::<crate::model::HealthConfig>(v).ok());
    let img = Image {
        name: name.clone(),
        rootfs: loaded.rootfs.to_string_lossy().into_owned(),
        arch: guest_of(loaded.arch),
        cmd: loaded.cmd,
        env: loaded.env,
        entrypoint: loaded.entrypoint,
        workdir: loaded.workdir,
        user: loaded.user,
        exposed_ports: loaded.exposed_ports,
        stop_signal: loaded.stop_signal,
        img_volumes: loaded.img_volumes,
        healthcheck,
        created: now_secs(),
        ..Default::default()
    };
    register_image(&a, img).await;
    crate::events::emit_event(&a.events, "image", "load", &name, json!({"name": name}));
    Json(LoadResponse {
        stream: format!("Loaded image: {}", repo_tag(&name)),
    })
    .into_response()
}

/// `docker load` failure -> 500 + a Docker-shaped `{"message": …}` error body.
fn load_err(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"message": msg})),
    )
        .into_response()
}
