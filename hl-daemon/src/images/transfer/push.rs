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
    // The route `name` is collapsed to the bare image (e.g. `myorg/myapp` -> `myapp`), so match on
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
    let os = img.arch.os().to_string(); // "linux"
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
    // Assemble the FULL OCI config.config object so the pushed image starts + inspects like the local one
    // (Docker push preserves entrypoint/env/user/workdir/ports/labels/volumes/stop-signal/healthcheck,
    // not just Cmd).
    let config_obj = oci_config_from_image(&img);
    let rootfs = img.rootfs.clone();
    let res = tokio::task::spawn_blocking(move || {
        Client::new(iref, creds)
            .push(
                std::path::Path::new(&rootfs),
                &config_obj,
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
/// returned `(digest, manifest_size, layer_size)` instead of just the digest, hl could emit Docker's
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

/// Assemble the OCI image `config.config` object from a stored [`Image`] so `docker push` uploads the
/// image's FULL runtime metadata — Docker preserves `Entrypoint`, `Env`, `WorkingDir`, `User`,
/// `ExposedPorts`, `Labels`, `StopSignal`, `Volumes`, and `Healthcheck`, not just `Cmd`. Unset/empty
/// fields are omitted (Docker only emits keys the image actually sets); `ExposedPorts`/`Volumes` are OCI
/// sets (`{key: {}}`), `Labels` is a sorted object for deterministic output, and `Healthcheck` reuses the
/// docker-shaped `HealthConfig` serialization.
fn oci_config_from_image(img: &Image) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let mut c = Map::new();
    if !img.cmd.is_empty() {
        c.insert("Cmd".into(), json!(img.cmd));
    }
    if !img.entrypoint.is_empty() {
        c.insert("Entrypoint".into(), json!(img.entrypoint));
    }
    if !img.env.is_empty() {
        c.insert("Env".into(), json!(img.env));
    }
    if !img.workdir.is_empty() {
        c.insert("WorkingDir".into(), json!(img.workdir));
    }
    if !img.user.is_empty() {
        c.insert("User".into(), json!(img.user));
    }
    if !img.exposed_ports.is_empty() {
        let ports: Map<String, Value> =
            img.exposed_ports.iter().map(|p| (p.clone(), json!({}))).collect();
        c.insert("ExposedPorts".into(), Value::Object(ports));
    }
    if !img.labels.is_empty() {
        let mut kv: Vec<(&String, &String)> = img.labels.iter().collect();
        kv.sort_by(|a, b| a.0.cmp(b.0));
        let m: Map<String, Value> = kv.into_iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        c.insert("Labels".into(), Value::Object(m));
    }
    if !img.stop_signal.is_empty() {
        c.insert("StopSignal".into(), json!(img.stop_signal));
    }
    if !img.img_volumes.is_empty() {
        let vols: Map<String, Value> =
            img.img_volumes.iter().map(|v| (v.clone(), json!({}))).collect();
        c.insert("Volumes".into(), Value::Object(vols));
    }
    if let Some(hc) = &img.healthcheck {
        if let Ok(v) = serde_json::to_value(hc) {
            c.insert("Healthcheck".into(), v);
        }
    }
    Value::Object(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Push must carry the FULL OCI config, not just Cmd: an image with entrypoint/env/user/workdir/
    // ports/labels/stop-signal/volumes/healthcheck produces a config object that preserves each field.
    #[test]
    fn oci_config_from_image_preserves_runtime_metadata() {
        let mut img = Image {
            cmd: vec!["/bin/sh".into()],
            entrypoint: vec!["/entry".into()],
            env: vec!["A=1".into(), "B=2".into()],
            workdir: "/work".into(),
            user: "1000:1000".into(),
            exposed_ports: vec!["8080/tcp".into()],
            stop_signal: "SIGINT".into(),
            img_volumes: vec!["/data".into()],
            ..Default::default()
        };
        img.labels.insert("org.opencontainers.image.title".into(), "demo".into());
        img.healthcheck = Some(crate::model::HealthConfig {
            test: vec!["CMD".into(), "true".into()],
            ..Default::default()
        });

        let c = oci_config_from_image(&img);
        assert_eq!(c["Cmd"], serde_json::json!(["/bin/sh"]));
        assert_eq!(c["Entrypoint"], serde_json::json!(["/entry"]));
        assert_eq!(c["Env"], serde_json::json!(["A=1", "B=2"]));
        assert_eq!(c["WorkingDir"], "/work");
        assert_eq!(c["User"], "1000:1000");
        assert_eq!(c["ExposedPorts"], serde_json::json!({"8080/tcp": {}}));
        assert_eq!(c["Labels"]["org.opencontainers.image.title"], "demo");
        assert_eq!(c["StopSignal"], "SIGINT");
        assert_eq!(c["Volumes"], serde_json::json!({"/data": {}}));
        assert_eq!(c["Healthcheck"]["Test"], serde_json::json!(["CMD", "true"]));
    }

    // Unset fields are omitted so a bare image reproduces the old `{"Cmd": [...]}`-only shape.
    #[test]
    fn oci_config_from_image_omits_unset_fields() {
        let img = Image {
            cmd: vec!["/bin/sh".into()],
            ..Default::default()
        };
        let c = oci_config_from_image(&img);
        assert!(c.get("Entrypoint").is_none());
        assert!(c.get("Env").is_none());
        assert!(c.get("Healthcheck").is_none());
        assert_eq!(c["Cmd"], serde_json::json!(["/bin/sh"]));
    }
}
