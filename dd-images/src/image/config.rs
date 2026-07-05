//! Small helpers over image references and OCI config blobs (store paths, ref parsing, config arrays,
//! and the pure OCI `config.config.*` field extractors + `repository:tag` / default-command helpers the
//! image store needs). All runtime-agnostic: they read a config `Value` or a rootfs path and return
//! plain data, so both `dd-images` and the daemon share one implementation.

use crate::registry::ImageRef;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// The store path component for a reference: its canonical form with `/` and `:` flattened to `_`.
pub fn safe_name(r: &ImageRef) -> String {
    r.canonical().replace(['/', ':'], "_")
}

/// Parse `from_image` into an [`ImageRef`], overriding the tag with `tag` when non-empty.
pub fn image_ref(from_image: &str, tag: &str) -> ImageRef {
    let mut r = ImageRef::parse(from_image);
    if !tag.is_empty() {
        r.tag = tag.to_string();
    }
    r
}

/// A string array at `config.config.<key>` of an OCI config blob, flattened to `Vec<String>`.
pub fn config_strs(config: &Value, key: &str) -> Vec<String> {
    config["config"][key]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// The keys of the `config.config.ExposedPorts` object of an OCI image config blob (e.g. `"5432/tcp"`),
/// as a sorted `Vec<String>`. OCI stores exposed ports as a set (object with empty values); we keep just
/// the keys and re-materialize the `{port: {}}` object at inspect time. Absent -> empty.
pub fn config_exposed_ports(config: &Value) -> Vec<String> {
    let mut v: Vec<String> = config["config"]["ExposedPorts"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// The `config.config.Labels` object of an OCI image config blob as a `HashMap` (absent/non-string -> empty).
pub fn config_labels(config: &Value) -> HashMap<String, String> {
    config["config"]["Labels"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The `config.config.StopSignal` of an OCI image config (e.g. `"SIGQUIT"`) — the signal `docker stop`
/// sends this image's container. Empty when the image sets none (the container then defaults to SIGTERM).
pub fn config_stop_signal(config: &Value) -> String {
    config["config"]["StopSignal"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// The keys of `config.config.Volumes` (an OCI set: object with empty values) — the dirs the image
/// declared via `VOLUME`, each of which gets an ANONYMOUS volume at `docker run`. Sorted; absent -> empty.
pub fn config_volumes(config: &Value) -> Vec<String> {
    let mut v: Vec<String> = config["config"]["Volumes"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A `repository:tag` string with exactly one tag — discovered images carry a bare name (`busybox`),
/// pulled ones already include the tag (`busybox:latest`); append `:latest` only when absent.
pub fn repo_tag(name: &str) -> String {
    let last = name.rsplit('/').next().unwrap_or(name);
    if last.contains(':') {
        name.to_string()
    } else {
        format!("{name}:latest")
    }
}

/// The tag portion of an image reference, defaulting to `latest` when none is given. A `:port` inside a
/// registry host (`localhost:5000/foo`) is NOT a tag — only a final `:tag` with no slash after the colon
/// counts: `ubuntu:24.04` -> `24.04`, `ubuntu` -> `latest`, `localhost:5000/foo` -> `latest`. Lets `rmi`
/// (and `push`) tell `ubuntu:24.04` apart from `ubuntu` (`:latest`) so an untag is tag-precise.
pub fn ref_tag(name: &str) -> String {
    match name.rsplit_once(':') {
        Some((_, t)) if !t.contains('/') => t.to_string(),
        _ => "latest".to_string(),
    }
}

/// The bare repository name of a docker image reference, ignoring registry, namespace and tag/digest:
/// `docker.io/library/ubuntu:latest` -> `ubuntu`, `library/ubuntu` -> `ubuntu`, `ubuntu:22.04` -> `ubuntu`.
/// Lets `docker run ubuntu` match an image discovered/tagged as `ubuntu` regardless of how docker
/// canonicalizes the reference. A loose display/match key — prefer [`ref_repo`] for identity decisions.
pub fn ref_name(s: &str) -> &str {
    let last = s.rsplit('/').next().unwrap_or(s);
    last.split('@')
        .next()
        .unwrap_or(last)
        .split(':')
        .next()
        .unwrap_or(last)
}

/// The FULLY-QUALIFIED canonical repository of an image reference — registry + namespace + name, tag
/// stripped, with Docker Hub's implicit `library/` namespace made explicit. This is the correct key for
/// "is this the same image?" because it distinguishes repositories that merely share a final path
/// component: `nginx`, `library/nginx`, `docker.io/library/nginx:1.25` all map to
/// `registry-1.docker.io/library/nginx`, but `linuxserver/nginx` maps to
/// `registry-1.docker.io/linuxserver/nginx`. Using the bare basename ([`ref_name`]) instead makes
/// `docker run nginx` resolve to a locally-present `linuxserver/nginx` — a cross-repo collision.
pub fn ref_repo(s: &str) -> String {
    let r = ImageRef::parse(s);
    format!("{}/{}", r.registry, r.repository)
}

/// Fallback default command for an image whose config has no Cmd: prefer /bin/sh, else /bin/bash.
pub fn default_shell(rootfs: &Path) -> Vec<String> {
    for sh in ["/bin/sh", "/bin/bash"] {
        if rootfs.join(sh.trim_start_matches('/')).exists() {
            return vec![sh.to_string()];
        }
    }
    vec!["/bin/sh".to_string()]
}
