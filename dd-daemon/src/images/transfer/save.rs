//! `docker save` -- stream a tar of the image's `rootfs/` + a `dd-manifest.json` sidecar.
use super::*;

#[derive(Deserialize)]
pub(crate) struct SaveQ {
    names: Option<String>,
}

/// GET /images/get?names=<name> -- `docker save`. Streams a tar of the image's `rootfs/` directory
/// plus a `dd-manifest.json` naming the image, as `application/x-tar`.
pub(crate) async fn image_save(State(a): State<App>, Query(q): Query<SaveQ>) -> Response {
    let names = q.names.unwrap_or_default();
    if names.is_empty() {
        return bad_request("names is required");
    }
    let img = {
        let g = a.inner.lock().await;
        g.images
            .iter()
            .find(|i| repo_tag(&i.name) == names || ref_name(&i.name) == ref_name(&names))
            .cloned()
    };
    let Some(img) = img else {
        return no_such_image(&names);
    };
    // The `macos` image is the live host filesystem (rootfs ~ `/`); taring it would be catastrophic.
    if img.name == "macos" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorMessage {
                message: "cannot save the host `macos` image".into(),
            }),
        )
            .into_response();
    }
    // dd-images owns the archive format (tar of `rootfs/` + a `dd-manifest.json` sidecar); the handler just
    // maps the store `Image` onto the manifest and streams the bytes back.
    let manifest = dd_images::Manifest {
        name: img.name.clone(),
        cmd: img.cmd.clone(),
        env: img.env.clone(),
        entrypoint: img.entrypoint.clone(),
        workdir: img.workdir.clone(),
        user: img.user.clone(),
        exposed_ports: img.exposed_ports.clone(),
        os: (img.arch.os() == "darwin").then(|| "darwin".to_string()),
        ..Default::default()
    };
    let rootfs = std::path::PathBuf::from(&img.rootfs);
    match dd_images::Store::new(&a.images_dir).save_archive(&rootfs, &manifest) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/x-tar")
            .body(Body::from(bytes))
            .unwrap(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorMessage { message: e }),
        )
            .into_response(),
    }
}
