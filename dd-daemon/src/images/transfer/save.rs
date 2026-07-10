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
    // Resolve through the repo-qualified `find_image` (NOT a bare-basename match): `docker save nginx`
    // must not serialize an unrelated `linuxserver/nginx` that merely shares the basename, and an explicit
    // missing tag is a not-found rather than a fallback to a sibling tag.
    let img = {
        let g = a.inner.lock().await;
        find_image(&g.images, &names).cloned()
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
        // Record the linux instruction set so an ELF-less rootfs (scratch/distroless linux/amd64) restores
        // its arch on load instead of being ELF-sniffed down to arm64. Labels carry through too.
        arch: (img.arch.os() == "linux").then(|| img.arch.arch().to_string()),
        labels: img.labels.clone(),
        ..Default::default()
    };
    let rootfs = std::path::PathBuf::from(&img.rootfs);
    match dd_images::Store::new(&a.images_dir)
        .save_archive(&rootfs, &manifest)
        .map_err(|e| e.to_string())
    {
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

#[cfg(test)]
mod tests {
    use crate::model::Image;
    use crate::util::find_image;

    fn img(name: &str) -> Image {
        Image { name: name.into(), rootfs: format!("/store/{name}"), ..Default::default() }
    }

    // "docker save nginx Can Serialize linuxserver/nginx" (P1): save resolves through the repo-qualified
    // find_image, so a bare official name must not serialize an unrelated same-basename repository.
    #[test]
    fn save_bare_name_does_not_match_foreign_repo_basename() {
        let imgs = vec![img("linuxserver/nginx:latest")];
        assert!(
            find_image(&imgs, "nginx").is_none(),
            "save nginx must not resolve linuxserver/nginx"
        );
        // The full repository still resolves to itself.
        assert_eq!(
            find_image(&imgs, "linuxserver/nginx").unwrap().name,
            "linuxserver/nginx:latest"
        );
    }
}
