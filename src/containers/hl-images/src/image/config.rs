//! Small helpers over image references and OCI config blobs (store paths, ref parsing, config arrays,
//! and the pure OCI `config.config.*` field extractors + `repository:tag` / default-command helpers the
//! image store needs). All runtime-agnostic: they read a config `Value` or a rootfs path and return
//! plain data, so both `hl-images` and the daemon share one implementation.

use crate::registry::ImageRef;
use serde_json::Value;
use std::collections::HashMap;

/// Encode an arbitrary image name into a SINGLE, injective, filesystem-safe path component for use as a
/// store directory. The old scheme flattened both `/` and `:` to `_`, which was NOT injective —
/// `owner/app:1_2` and `owner/app_1:2` both became `owner_app_1_2` and collided. This percent-encodes the
/// escape char and the two separators (`%`->`%25`, `/`->`%2F`, `:`->`%3A`), which is reversible and keeps
/// distinct refs distinct. Because the result contains no `/` and (via the `.`/`..`/empty guard) is never
/// a traversal component, it also cannot escape the store root when appended to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Key(String);

impl Key {
    /// Encodes an arbitrary store name as one injective path component.
    pub fn from_name(name: &str) -> Self {
        let encoded = name
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace(':', "%3A");
        // A bare `.`/`..`/empty component would still traverse or alias the store dir; make it inert.
        Self(match encoded.as_str() {
            "" => "%2E".to_string(),
            "." => "%2E".to_string(),
            ".." => "%2E%2E".to_string(),
            _ => encoded,
        })
    }

    /// The store path component for a reference: its canonical form encoded via [`encode_store_component`]
    /// (injective, filesystem-safe, reversible — `/`->`%2F`, `:`->`%3A`).
    /// Encodes a canonical image reference for store layout.
    pub fn from_reference(reference: &ImageRef) -> Self {
        Self::from_name(&reference.canonical())
    }

    /// Returns the encoded component.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Parse `from_image` into an [`ImageRef`], overriding the tag with `tag` when non-empty.
/// A typed view over the runtime fields of an OCI image configuration.
#[derive(Clone, Copy, Debug)]
pub struct ImageConfig<'a> {
    value: &'a Value,
}

impl<'a> From<&'a Value> for ImageConfig<'a> {
    fn from(value: &'a Value) -> Self {
        Self { value }
    }
}

impl ImageConfig<'_> {
    fn strings(&self, key: &str) -> Vec<String> {
        self.value["config"][key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// OCI command arguments, ignoring malformed non-string members for compatibility.
    pub fn command(&self) -> Vec<String> {
        self.strings("Cmd")
    }
    /// OCI entrypoint arguments, ignoring malformed non-string members for compatibility.
    pub fn entrypoint(&self) -> Vec<String> {
        self.strings("Entrypoint")
    }
    /// OCI environment entries, ignoring malformed non-string members for compatibility.
    pub fn environment(&self) -> Vec<String> {
        self.strings("Env")
    }
    /// Configured working directory, or empty when absent/malformed.
    pub fn working_directory(&self) -> String {
        self.value["config"]["WorkingDir"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
    /// Configured user, or empty when absent/malformed.
    pub fn user(&self) -> String {
        self.value["config"]["User"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }

    /// Sorted exposed-port set.
    pub fn exposed_ports(&self) -> Vec<String> {
        let mut v: Vec<String> = self.value["config"]["ExposedPorts"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// The `config.config.Labels` object of an OCI image config blob as a `HashMap` (absent/non-string -> empty).
    /// String-valued image labels; malformed values are ignored.
    pub fn labels(&self) -> HashMap<String, String> {
        self.value["config"]["Labels"]
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
    /// Configured stop signal, or empty when absent/malformed.
    pub fn stop_signal(&self) -> String {
        self.value["config"]["StopSignal"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// The keys of `config.config.Volumes` (an OCI set: object with empty values) — the dirs the image
    /// declared via `VOLUME`, each of which gets an ANONYMOUS volume at `docker run`. Sorted; absent -> empty.
    /// Sorted declared-volume set.
    pub fn volumes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.value["config"]["Volumes"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

/// A `repository:tag` string with exactly one tag — discovered images carry a bare name (`busybox`),
/// pulled ones already include the tag (`busybox:latest`); append `:latest` only when absent.
/// The tag portion of an image reference, defaulting to `latest` when none is given. A `:port` inside a
/// registry host (`localhost:5000/foo`) is NOT a tag — only a final `:tag` with no slash after the colon
/// counts: `ubuntu:24.04` -> `24.04`, `ubuntu` -> `latest`, `localhost:5000/foo` -> `latest`. Lets `rmi`
/// (and `push`) tell `ubuntu:24.04` apart from `ubuntu` (`:latest`) so an untag is tag-precise.
/// The bare repository name of a docker image reference, ignoring registry, namespace and tag/digest:
/// `docker.io/library/ubuntu:latest` -> `ubuntu`, `library/ubuntu` -> `ubuntu`, `ubuntu:22.04` -> `ubuntu`.
/// Lets `docker run ubuntu` match an image discovered/tagged as `ubuntu` regardless of how docker
/// canonicalizes the reference. A loose display/match key, not an identity.
/// The FULLY-QUALIFIED canonical repository of an image reference — registry + namespace + name, tag
/// stripped, with Docker Hub's implicit `library/` namespace made explicit. This is the correct key for
/// "is this the same image?" because it distinguishes repositories that merely share a final path
/// component: `nginx`, `library/nginx`, `docker.io/library/nginx:1.25` all map to
/// `registry-1.docker.io/library/nginx`, but `linuxserver/nginx` maps to
/// `registry-1.docker.io/linuxserver/nginx`. Using the bare basename instead makes
/// `docker run nginx` resolve to a locally-present `linuxserver/nginx` — a cross-repo collision.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ref_tag_cases() {
        // explicit tag
        assert_eq!(ImageRef::from("ubuntu:24.04").tag, "24.04");
        // default tag when none
        assert_eq!(ImageRef::from("ubuntu").tag, "latest");
        // a `:port` inside a registry host is NOT a tag (slash after the colon)
        assert_eq!(ImageRef::from("localhost:5000/foo").tag, "latest");
        assert_eq!(ImageRef::from("localhost:5000/foo:1.2").tag, "1.2");
    }

    #[test]
    fn ref_name_cases() {
        assert_eq!(
            ImageRef::from("docker.io/library/ubuntu:latest").name(),
            "ubuntu"
        );
        assert_eq!(ImageRef::from("library/ubuntu").name(), "ubuntu");
        assert_eq!(ImageRef::from("ubuntu:22.04").name(), "ubuntu");
        assert_eq!(ImageRef::from("ubuntu").name(), "ubuntu");
        assert_eq!(ImageRef::from("repo/app@sha256:deadbeef").name(), "app");
    }

    #[test]
    fn ref_repo_normalizes_registry_and_namespace() {
        // Docker Hub's implicit `library/` namespace made explicit; short/prefixed forms all collapse.
        assert_eq!(
            ImageRef::from("nginx").repository_identity(),
            "registry-1.docker.io/library/nginx"
        );
        assert_eq!(
            ImageRef::from("library/nginx").repository_identity(),
            "registry-1.docker.io/library/nginx"
        );
        assert_eq!(
            ImageRef::from("docker.io/library/nginx:1.25").repository_identity(),
            "registry-1.docker.io/library/nginx"
        );
        // a distinct namespace stays distinct (no cross-repo collision)
        assert_eq!(
            ImageRef::from("linuxserver/nginx").repository_identity(),
            "registry-1.docker.io/linuxserver/nginx"
        );
        // an explicit non-Hub registry is preserved
        assert_eq!(
            ImageRef::from("ghcr.io/owner/app:v2").repository_identity(),
            "ghcr.io/owner/app"
        );
    }

    #[test]
    fn repo_tag_appends_latest_only_when_absent() {
        assert_eq!(ImageRef::from("busybox").short(), "busybox:latest");
        assert_eq!(ImageRef::from("busybox:latest").short(), "busybox:latest");
        assert_eq!(ImageRef::from("busybox:1.36").short(), "busybox:1.36");
        // a `:port` in the host is not a tag -> still needs `:latest` appended
        assert_eq!(
            ImageRef::from("localhost:5000/foo").short(),
            "localhost:5000/foo:latest"
        );
        // a tag on the final path component is detected
        assert_eq!(ImageRef::from("foo/bar:1").short(), "foo/bar:1");
    }

    #[test]
    fn safe_name_encodes_canonical() {
        let r = ImageRef::from("nginx");
        // canonical() == "docker.io/library/nginx:latest"; `/`->%2F, `:`->%3A (injective + reversible).
        assert_eq!(
            Key::from_reference(&r).as_str(),
            "docker.io%2Flibrary%2Fnginx%3Alatest"
        );
    }

    // Finding 7 — encode_store_component is injective for refs that collided under the old flatten-to-`_`.
    #[test]
    fn encode_store_component_is_injective() {
        // These two both flattened to `owner_app_1_2` before; now distinct.
        assert_ne!(
            Key::from_name("owner/app:1_2").as_str(),
            Key::from_name("owner/app_1:2").as_str()
        );
        // A `%` in the source is escaped so it can't forge an encoded separator.
        assert_ne!(
            Key::from_name("a%2Fb").as_str(),
            Key::from_name("a/b").as_str()
        );
        // No path separator survives, so the result is always a single component.
        assert!(!Key::from_name("a/b:c").as_str().contains('/'));
        // Traversal / empty names are made inert.
        assert_eq!(Key::from_name("..").as_str(), "%2E%2E");
        assert_eq!(Key::from_name(".").as_str(), "%2E");
        assert_eq!(Key::from_name("").as_str(), "%2E");
        // Ordinary underscore-only names are untouched (readable + stable).
        assert_eq!(Key::from_name("busybox_latest").as_str(), "busybox_latest");
    }

    #[test]
    fn config_exposed_ports_sorted_keys() {
        let c = json!({"config": {"ExposedPorts": {"80/tcp": {}, "5432/tcp": {}}}});
        assert_eq!(
            ImageConfig::from(&c).exposed_ports(),
            vec!["5432/tcp", "80/tcp"]
        );
        // absent -> empty
        assert!(ImageConfig::from(&json!({"config": {}}))
            .exposed_ports()
            .is_empty());
    }

    #[test]
    fn config_labels_map() {
        let c = json!({"config": {"Labels": {"a": "b", "maintainer": "acme"}}});
        let m = ImageConfig::from(&c).labels();
        assert_eq!(m.get("a").map(String::as_str), Some("b"));
        assert_eq!(m.get("maintainer").map(String::as_str), Some("acme"));
        assert_eq!(m.len(), 2);
        // absent -> empty
        assert!(ImageConfig::from(&json!({"config": {}}))
            .labels()
            .is_empty());
    }

    #[test]
    fn config_volumes_sorted_keys() {
        let c = json!({"config": {"Volumes": {"/var": {}, "/data": {}}}});
        assert_eq!(ImageConfig::from(&c).volumes(), vec!["/data", "/var"]);
        // absent -> empty
        assert!(ImageConfig::from(&json!({"config": {}}))
            .volumes()
            .is_empty());
    }

    #[test]
    fn config_stop_signal_str() {
        let c = json!({"config": {"StopSignal": "SIGQUIT"}});
        assert_eq!(ImageConfig::from(&c).stop_signal(), "SIGQUIT");
        // absent -> empty
        assert_eq!(ImageConfig::from(&json!({"config": {}})).stop_signal(), "");
    }

    #[test]
    fn config_strs_array() {
        let c = json!({"config": {"Env": ["A=1", "B=2"]}});
        assert_eq!(ImageConfig::from(&c).environment(), vec!["A=1", "B=2"]);
        // missing key -> empty
        assert!(ImageConfig::from(&c).command().is_empty());
    }
}
