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
}
