//! Small helpers over image references and OCI config blobs (store paths, ref parsing, config arrays,
//! and the pure OCI `config.config.*` field extractors + `repository:tag` / default-command helpers the
//! image store needs). All runtime-agnostic: they read a config `Value` or a rootfs path and return
//! plain data, so both `dd-images` and the daemon share one implementation.

use crate::registry::ImageRef;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Encode an arbitrary image name into a SINGLE, injective, filesystem-safe path component for use as a
/// store directory. The old scheme flattened both `/` and `:` to `_`, which was NOT injective —
/// `owner/app:1_2` and `owner/app_1:2` both became `owner_app_1_2` and collided. This percent-encodes the
/// escape char and the two separators (`%`->`%25`, `/`->`%2F`, `:`->`%3A`), which is reversible and keeps
/// distinct refs distinct. Because the result contains no `/` and (via the `.`/`..`/empty guard) is never
/// a traversal component, it also cannot escape the store root when appended to it.
pub(crate) fn encode_store_component(name: &str) -> String {
    let encoded = name
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace(':', "%3A");
    // A bare `.`/`..`/empty component would still traverse or alias the store dir; make it inert.
    match encoded.as_str() {
        "" => "%2E".to_string(),
        "." => "%2E".to_string(),
        ".." => "%2E%2E".to_string(),
        _ => encoded,
    }
}

/// The store path component for a reference: its canonical form encoded via [`encode_store_component`]
/// (injective, filesystem-safe, reversible — `/`->`%2F`, `:`->`%3A`).
pub fn safe_name(r: &ImageRef) -> String {
    encode_store_component(&r.canonical())
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
    crate::registry::split_tag(name).1
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ref_tag_cases() {
        // explicit tag
        assert_eq!(ref_tag("ubuntu:24.04"), "24.04");
        // default tag when none
        assert_eq!(ref_tag("ubuntu"), "latest");
        // a `:port` inside a registry host is NOT a tag (slash after the colon)
        assert_eq!(ref_tag("localhost:5000/foo"), "latest");
        assert_eq!(ref_tag("localhost:5000/foo:1.2"), "1.2");
    }

    #[test]
    fn ref_name_cases() {
        assert_eq!(ref_name("docker.io/library/ubuntu:latest"), "ubuntu");
        assert_eq!(ref_name("library/ubuntu"), "ubuntu");
        assert_eq!(ref_name("ubuntu:22.04"), "ubuntu");
        assert_eq!(ref_name("ubuntu"), "ubuntu");
        assert_eq!(ref_name("repo/app@sha256:deadbeef"), "app");
    }

    #[test]
    fn ref_repo_normalizes_registry_and_namespace() {
        // Docker Hub's implicit `library/` namespace made explicit; short/prefixed forms all collapse.
        assert_eq!(ref_repo("nginx"), "registry-1.docker.io/library/nginx");
        assert_eq!(ref_repo("library/nginx"), "registry-1.docker.io/library/nginx");
        assert_eq!(
            ref_repo("docker.io/library/nginx:1.25"),
            "registry-1.docker.io/library/nginx"
        );
        // a distinct namespace stays distinct (no cross-repo collision)
        assert_eq!(
            ref_repo("linuxserver/nginx"),
            "registry-1.docker.io/linuxserver/nginx"
        );
        // an explicit non-Hub registry is preserved
        assert_eq!(ref_repo("ghcr.io/owner/app:v2"), "ghcr.io/owner/app");
    }

    #[test]
    fn repo_tag_appends_latest_only_when_absent() {
        assert_eq!(repo_tag("busybox"), "busybox:latest");
        assert_eq!(repo_tag("busybox:latest"), "busybox:latest");
        assert_eq!(repo_tag("busybox:1.36"), "busybox:1.36");
        // a `:port` in the host is not a tag -> still needs `:latest` appended
        assert_eq!(repo_tag("localhost:5000/foo"), "localhost:5000/foo:latest");
        // a tag on the final path component is detected
        assert_eq!(repo_tag("foo/bar:1"), "foo/bar:1");
    }

    #[test]
    fn safe_name_encodes_canonical() {
        let r = ImageRef::parse("nginx");
        // canonical() == "docker.io/library/nginx:latest"; `/`->%2F, `:`->%3A (injective + reversible).
        assert_eq!(safe_name(&r), "docker.io%2Flibrary%2Fnginx%3Alatest");
    }

    // Finding 7 — encode_store_component is injective for refs that collided under the old flatten-to-`_`.
    #[test]
    fn encode_store_component_is_injective() {
        // These two both flattened to `owner_app_1_2` before; now distinct.
        assert_ne!(
            encode_store_component("owner/app:1_2"),
            encode_store_component("owner/app_1:2")
        );
        // A `%` in the source is escaped so it can't forge an encoded separator.
        assert_ne!(encode_store_component("a%2Fb"), encode_store_component("a/b"));
        // No path separator survives, so the result is always a single component.
        assert!(!encode_store_component("a/b:c").contains('/'));
        // Traversal / empty names are made inert.
        assert_eq!(encode_store_component(".."), "%2E%2E");
        assert_eq!(encode_store_component("."), "%2E");
        assert_eq!(encode_store_component(""), "%2E");
        // Ordinary underscore-only names are untouched (readable + stable).
        assert_eq!(encode_store_component("busybox_latest"), "busybox_latest");
    }

    #[test]
    fn config_exposed_ports_sorted_keys() {
        let c = json!({"config": {"ExposedPorts": {"80/tcp": {}, "5432/tcp": {}}}});
        assert_eq!(config_exposed_ports(&c), vec!["5432/tcp", "80/tcp"]);
        // absent -> empty
        assert!(config_exposed_ports(&json!({"config": {}})).is_empty());
    }

    #[test]
    fn config_labels_map() {
        let c = json!({"config": {"Labels": {"a": "b", "maintainer": "acme"}}});
        let m = config_labels(&c);
        assert_eq!(m.get("a").map(String::as_str), Some("b"));
        assert_eq!(m.get("maintainer").map(String::as_str), Some("acme"));
        assert_eq!(m.len(), 2);
        // absent -> empty
        assert!(config_labels(&json!({"config": {}})).is_empty());
    }

    #[test]
    fn config_volumes_sorted_keys() {
        let c = json!({"config": {"Volumes": {"/var": {}, "/data": {}}}});
        assert_eq!(config_volumes(&c), vec!["/data", "/var"]);
        // absent -> empty
        assert!(config_volumes(&json!({"config": {}})).is_empty());
    }

    #[test]
    fn config_stop_signal_str() {
        let c = json!({"config": {"StopSignal": "SIGQUIT"}});
        assert_eq!(config_stop_signal(&c), "SIGQUIT");
        // absent -> empty
        assert_eq!(config_stop_signal(&json!({"config": {}})), "");
    }

    #[test]
    fn config_strs_array() {
        let c = json!({"config": {"Env": ["A=1", "B=2"]}});
        assert_eq!(config_strs(&c, "Env"), vec!["A=1", "B=2"]);
        // missing key -> empty
        assert!(config_strs(&c, "Cmd").is_empty());
    }
}
