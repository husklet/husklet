//! Registry config-refresh + image-build path: the fresh pull, the layers-cached config refresh of an
//! already-present tag, the shared [`Image`] builder from an OCI config blob, and the healthcheck map.
use super::*;
use crate::registry::{Client, Credentials, ImageRef, PullEvent};
use hl_jit::Guest;
use serde_json::json;

/// Best-effort refresh of an already-cached image's stored config on a re-pull. Runs the
/// blocking registry config re-fetch off the async runtime, then replaces the matching store entry's
/// run-config in place (preserving its on-disk rootfs). A miss (rootfs absent, registry unreachable,
/// or no matching store slot) is silently ignored — the cached entry is left as-is and the pull still
/// reports the tag as up to date, so an offline `docker pull` of a present image never breaks.
pub(super) async fn refresh_local_config(
    a: &App,
    name: &str,
    tag: &str,
    want: String,
    creds: Credentials,
    archs: Vec<&'static str>,
) {
    let (dir, nm, tg) = (a.images_dir.clone(), name.to_string(), tag.to_string());
    let fresh =
        tokio::task::spawn_blocking(move || refresh_image_config(&dir, &nm, &tg, creds, &archs))
            .await
            .ok()
            .flatten();
    let Some(fresh) = fresh else { return };
    let mut g = a.inner.lock().await;
    if let Some(slot) = g
        .images
        .iter_mut()
        .find(|i| repo_tag(&i.name) == want && docker_arch(i.arch) == docker_arch(fresh.arch))
    {
        *slot = fresh;
    }
}

/// Pull an image from its registry (any registry) and unpack it under `<images_dir>/<safe>/rootfs`,
/// preferring the linux/arm64 variant (native; falls back to amd64). Returns the registered [`Image`].
pub(crate) fn pull_image(
    images_dir: &str,
    from_image: &str,
    tag: &str,
    creds: Credentials,
    archs: &[&str],
    progress: &mut dyn FnMut(PullEvent),
) -> Result<Image, String> {
    // dd-images owns the pull + rootfs unpack; the daemon just maps the result onto its Docker model.
    let li = hl_images::Store::new(images_dir)
        .pull_archs(from_image, tag, creds, archs, progress)
        .map_err(|e| e.to_string())?;
    Ok(image_from_config(images_dir, &li.iref, &li.config, &li.rootfs))
}

/// Re-fetch an already-cached image's run config from its registry and rebuild its [`Image`] WITHOUT
/// re-downloading the layers. `docker pull <img>:<tag>` on a tag that is already present
/// must still refresh the stored Entrypoint/Cmd/Env/WorkingDir — otherwise a stale store entry (e.g.
/// one discovered from disk with the `/bin/sh` fallback) makes a later bare `docker run <img>` launch
/// `/bin/sh` instead of the image's real entrypoint. Best-effort: returns `None` when the rootfs isn't
/// actually on disk (let a real pull handle it) or the registry is unreachable (keep the cached entry).
pub(crate) fn refresh_image_config(
    images_dir: &str,
    from_image: &str,
    tag: &str,
    creds: Credentials,
    archs: &[&str],
) -> Option<Image> {
    let iref = image_ref(from_image, tag);
    let rootfs = std::path::PathBuf::from(format!("{images_dir}/{}/rootfs", safe_name(&iref)));
    if !rootfs.is_dir() {
        return None;
    }
    let config = Client::new(iref.clone(), creds).fetch_config(archs).ok()?;
    // A registry that answered but gave no `config` object tells us nothing to refresh with — don't
    // clobber the cached entry with an empty config.
    if !config.get("config").map(|c| c.is_object()).unwrap_or(false) {
        return None;
    }
    Some(image_from_config(images_dir, &iref, &config, &rootfs))
}

/// Build a registered [`Image`] from a freshly-fetched OCI config blob + an already-unpacked rootfs,
/// and (re)write the `dd-image.json` sidecar so the config survives a daemon restart. Shared by
/// `pull_image` (fresh pull) and `refresh_image_config` (re-pull of a cached tag) so both derive
/// Cmd/Entrypoint/Env/WorkingDir/User/etc. identically.
pub(crate) fn image_from_config(
    images_dir: &str,
    iref: &ImageRef,
    config: &Value,
    rootfs: &std::path::Path,
) -> Image {
    // Distroless/scratch images carry no ELF/Mach-O to sniff, so the rootfs scan comes up empty.
    // Fall back to the config's `architecture`+`os`, then to native arm64 — never fail on undetectable arch.
    let arch = detect_arch(rootfs)
        .or_else(|| manifest_arch(config))
        .unwrap_or(Guest::LinuxAarch64);
    let darwin = arch.os() == "darwin";
    // Keep Entrypoint and Cmd *separate* (NOT flattened like `config_cmd`) so docker's override semantics
    // survive the round-trip — `containers_create` rebuilds argv = entrypoint ++ cmd and `--entrypoint`/CMD
    // overrides act on the right half (see containers.rs).
    let entrypoint = config_strs(config, "Entrypoint");
    let env = config_strs(config, "Env");
    let workdir = config["config"]["WorkingDir"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let user = config["config"]["User"].as_str().unwrap_or("").to_string();
    let exposed_ports = config_exposed_ports(config);
    let labels = config_labels(config);
    // Lifecycle/volume image config a container inherits at run (Moby §6/§8).
    let stop_signal = config_stop_signal(config);
    let img_volumes = config_volumes(config);
    let healthcheck = config_healthcheck(config);
    // A pulled macOS image's `dd-image.json` sidecar doesn't survive the registry round-trip and its
    // userland shell lives on the in-jail PATH (`/profile/bin/bash`), not `/bin/sh` — so default a
    // darwin image to a bare `bash` (resolved via PATH by the darwinjail) rather than `/bin/sh`. Only fall
    // back when the config supplies neither Entrypoint nor Cmd (an entrypoint-only image keeps empty cmd).
    let mut cmd = config_strs(config, "Cmd");
    if cmd.is_empty() && entrypoint.is_empty() {
        cmd = if darwin {
            vec!["bash".into()]
        } else {
            default_shell(rootfs)
        };
    }
    let name = iref.short();
    // Record name + the full OCI run config (cmd/env/entrypoint/workdir, +os for darwin) so the image keeps
    // its identity AND its entrypoint/env/workdir across a daemon restart (the dir name alone doesn't
    // round-trip -- e.g. "docker.io_library_alpine_latest"). Mirrors the `docker load` path (`image_load`).
    let mut meta = json!({ "name": name.clone(), "cmd": cmd.clone(), "env": env.clone(),
                           "entrypoint": entrypoint.clone(), "workdir": workdir.clone(),
                           "user": user.clone(), "exposed_ports": exposed_ports.clone(),
                           "stop_signal": stop_signal.clone(), "img_volumes": img_volumes.clone(),
                           "healthcheck": healthcheck.clone(),
                           "arch": arch.arch(), "os": arch.os() });
    if darwin {
        meta["os"] = json!("darwin");
    }
    let _ = std::fs::write(
        format!("{images_dir}/{}/dd-image.json", safe_name(iref)),
        meta.to_string(),
    );
    Image {
        name,
        rootfs: rootfs.to_string_lossy().into_owned(),
        arch,
        cmd,
        env,
        entrypoint,
        workdir,
        user,
        exposed_ports,
        labels,
        created: now_secs(),
        stop_signal,
        img_volumes,
        healthcheck,
        // A pulled image has no per-instruction build history / ONBUILD triggers of its own.
        history: Vec::new(),
        onbuild: Vec::new(),
    }
}

/// The `config.config.Healthcheck` of an OCI image config → [`HealthConfig`]. `Test=["NONE"]` (or absent)
/// yields None — no probe. Durations are the config's nanoseconds, carried through verbatim.
pub(crate) fn config_healthcheck(config: &Value) -> Option<crate::model::HealthConfig> {
    let hc = config["config"]["Healthcheck"].as_object()?;
    let test: Vec<String> = hc
        .get("Test")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if test.is_empty() || test.first().map(|s| s.as_str()) == Some("NONE") {
        return None;
    }
    let num = |k: &str| hc.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    Some(crate::model::HealthConfig {
        test,
        interval: num("Interval"),
        timeout: num("Timeout"),
        retries: num("Retries"),
        start_period: num("StartPeriod"),
    })
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use serde_json::json;

    // a re-pull of an already-present tag must refresh the stored config so `docker run` uses the
    // image's real Entrypoint/Cmd/Env/WorkingDir, NOT the `/bin/sh` fallback a stale/discovered entry
    // carries. `image_from_config` is the shared rebuilder both the fresh pull and the refresh use.
    #[test]
    fn image_from_config_uses_real_entrypoint_not_bin_sh() {
        let dir =
            std::env::temp_dir().join(format!("dd-refresh-{}-{}", std::process::id(), now_nanos()));
        let iref = ImageRef::parse("nginx:latest");
        let img_dir = dir.join(safe_name(&iref));
        let rootfs = img_dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let config = json!({
            "architecture": "arm64", "os": "linux",
            "config": {
                "Entrypoint": ["/docker-entrypoint.sh"],
                "Cmd": ["nginx", "-g", "daemon off;"],
                "Env": ["PATH=/usr/local/sbin:/usr/bin", "NGINX_VERSION=1.25.4"],
                "WorkingDir": "/work",
            }
        });

        let image = image_from_config(dir.to_str().unwrap(), &iref, &config, &rootfs);

        assert_eq!(image.name, "nginx:latest");
        assert_eq!(image.entrypoint, vec!["/docker-entrypoint.sh"]);
        assert_eq!(image.cmd, vec!["nginx", "-g", "daemon off;"]);
        assert!(image.env.iter().any(|e| e == "NGINX_VERSION=1.25.4"));
        assert_eq!(image.workdir, "/work");
        // The critical assertion: the run command is the image's, not the /bin/sh fallback.
        assert!(
            !image.cmd.iter().any(|c| c == "/bin/sh"),
            "refresh must not fall back to /bin/sh"
        );
        // The refreshed config is persisted to the sidecar so it survives a daemon restart.
        let side = std::fs::read_to_string(img_dir.join("dd-image.json")).unwrap();
        assert!(side.contains("docker-entrypoint.sh") && side.contains("nginx"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // An entrypoint-only image (no Cmd) keeps an empty Cmd rather than injecting /bin/sh.
    #[test]
    fn image_from_config_entrypoint_only_keeps_empty_cmd() {
        let dir = std::env::temp_dir().join(format!(
            "dd-refresh2-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let iref = ImageRef::parse("busybox:latest");
        let rootfs = dir.join(safe_name(&iref)).join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let config = json!({ "architecture": "arm64", "os": "linux",
                             "config": { "Entrypoint": ["/bin/busybox"] } });
        let image = image_from_config(dir.to_str().unwrap(), &iref, &config, &rootfs);
        assert_eq!(image.entrypoint, vec!["/bin/busybox"]);
        assert!(
            image.cmd.is_empty(),
            "entrypoint-only image must keep Cmd empty"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
