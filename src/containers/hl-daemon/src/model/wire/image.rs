use super::*;

#[derive(Clone, Default)]
pub(crate) struct Image {
    pub(crate) name: String,
    pub(crate) rootfs: String,
    pub(crate) arch: Guest,
    pub(crate) cmd: Vec<String>,
    // the rest of the OCI/Dockerfile config metadata a container inherits at run
    pub(crate) env: Vec<String>,           // "K=V" entries (ENV)
    pub(crate) entrypoint: Vec<String>,    // ENTRYPOINT (prepended to the command)
    pub(crate) workdir: String,            // WORKDIR / Config.WorkingDir
    pub(crate) user: String, // USER / Config.User — the image's default run user (uid[:gid]/name)
    pub(crate) exposed_ports: Vec<String>, // Config.ExposedPorts keys, e.g. "5432/tcp" (reported by inspect)
    pub(crate) created: i64,               // unix secs; image creation/discovery time
    pub(crate) labels: std::collections::HashMap<String, String>, // LABEL + build --label
    // Lifecycle/volume image config a container inherits at run (Moby §6/§8):
    pub(crate) stop_signal: String, // Config.StopSignal — the signal `docker stop` sends (nginx SIGQUIT, postgres SIGINT); "" ⇒ SIGTERM
    pub(crate) img_volumes: Vec<String>, // Config.Volumes keys — dirs that get an anonymous volume at run (postgres /var/lib/postgresql/data)
    pub(crate) healthcheck: Option<HealthConfig>, // Config.Healthcheck — the container HEALTHCHECK probe (None / Test=["NONE"] ⇒ no probe)
    // Per-instruction build history (`docker history`): one row per Dockerfile instruction, created at
    // build time. Empty ⇒ report the single synthetic "hl import" row (pulled/imported images).
    pub(crate) history: Vec<HistoryEntry>,
    // ONBUILD triggers this image carries (Dockerfile `ONBUILD X`), replayed when a child `FROM` this
    // image is built. Empty for a normal image.
    pub(crate) onbuild: Vec<String>,
}

impl Image {
    pub(crate) fn id(&self) -> String {
        let mut labels: Vec<_> = self.labels.iter().collect();
        labels.sort();
        let labels = labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let manifest = format!(
            "rootfs:{}\narch:{}\ncmd:{}\nentrypoint:{}\nenv:{}\nworkdir:{}\nuser:{}\nlabels:\n{}",
            self.rootfs,
            self.arch.arch(),
            self.cmd.join("\u{1}"),
            self.entrypoint.join("\u{1}"),
            self.env.join("\u{1}"),
            self.workdir,
            self.user,
            labels,
        );
        format!(
            "sha256:{}",
            hl_images::Sha256Digest::from_bytes(manifest.as_bytes())
        )
    }

    pub(crate) fn oci_config(&self) -> serde_json::Value {
        use serde_json::{json, Map, Value};
        let mut config = Map::new();
        for (key, value) in [
            ("Cmd", (!self.cmd.is_empty()).then(|| json!(self.cmd))),
            (
                "Entrypoint",
                (!self.entrypoint.is_empty()).then(|| json!(self.entrypoint)),
            ),
            ("Env", (!self.env.is_empty()).then(|| json!(self.env))),
            (
                "WorkingDir",
                (!self.workdir.is_empty()).then(|| json!(self.workdir)),
            ),
            ("User", (!self.user.is_empty()).then(|| json!(self.user))),
            (
                "StopSignal",
                (!self.stop_signal.is_empty()).then(|| json!(self.stop_signal)),
            ),
        ] {
            if let Some(value) = value {
                config.insert(key.into(), value);
            }
        }
        if !self.exposed_ports.is_empty() {
            config.insert(
                "ExposedPorts".into(),
                Value::Object(
                    self.exposed_ports
                        .iter()
                        .map(|p| (p.clone(), json!({})))
                        .collect(),
                ),
            );
        }
        if !self.labels.is_empty() {
            let mut labels: Vec<_> = self.labels.iter().collect();
            labels.sort_by(|a, b| a.0.cmp(b.0));
            config.insert(
                "Labels".into(),
                Value::Object(
                    labels
                        .into_iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
        }
        if !self.img_volumes.is_empty() {
            config.insert(
                "Volumes".into(),
                Value::Object(
                    self.img_volumes
                        .iter()
                        .map(|v| (v.clone(), json!({})))
                        .collect(),
                ),
            );
        }
        if let Some(value) = self
            .healthcheck
            .as_ref()
            .and_then(|h| serde_json::to_value(h).ok())
        {
            config.insert("Healthcheck".into(), value);
        }
        Value::Object(config)
    }

    pub(crate) fn score(&self) -> i32 {
        (!self.env.is_empty()) as i32 * 1000
            + (!self.entrypoint.is_empty()) as i32 * 10
            + (!self.workdir.is_empty()) as i32 * 5
            + self.labels.len() as i32
            + (self.cmd.len() != 1 || self.cmd[0] != "/bin/sh") as i32
    }
}

/// One `docker history` row (a build instruction). `empty_layer` is true for config-only instructions
/// (ENV/LABEL/CMD/…) that add no filesystem layer, matching Docker's history schema.
#[derive(Clone, Default)]
pub(crate) struct HistoryEntry {
    pub(crate) created: i64,
    pub(crate) created_by: String,
    pub(crate) empty_layer: bool,
}
