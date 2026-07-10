//! The daemon's axum route table: maps every Docker-Engine-API path to its handler and wires the
//! response-header + body-limit middleware. Handlers live in their per-resource modules
//! (`containers`, `images`, `volumes`, …); this module only assembles them into a `Router`.

use axum::{
    routing::{delete, get, post},
    Router,
};

use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::events;
use crate::http::{docker_headers, not_found};
use crate::images::*;
use crate::model::App;
use crate::networks::*;
use crate::system::*;
use crate::volumes::*;

/// Build the fully wired daemon router over the shared [`App`] state.
pub(crate) fn router(app: App) -> Router {
    Router::new()
        .route("/_ping", get(|| async { "OK" }))
        .route("/version", get(version))
        .route("/info", get(info))
        .route("/events", get(events::events))
        .route("/system/df", get(system_df))
        .route("/system/prune", post(system_prune))
        .route("/plugins", get(plugins_list))
        .route("/auth", post(auth))
        .route("/distribution/:name/json", get(distribution_inspect))
        .route("/images/json", get(images_json))
        .route("/images/create", post(images_create))
        .route("/images/get", get(image_save))
        .route("/images/load", post(image_load))
        .route("/images/search", get(image_search))
        .route("/images/prune", post(images_prune))
        .route("/images/:name/json", get(image_inspect))
        .route("/images/:name/history", get(image_history))
        .route("/images/:name/push", post(image_push))
        .route("/build", post(images_build))
        .route("/build/prune", post(build_prune))
        .route("/images/:name/tag", post(image_tag))
        .route("/images/:name", delete(image_delete))
        .route("/containers/json", get(containers_json))
        .route("/containers/create", post(containers_create))
        .route("/containers/prune", post(containers_prune))
        .route("/containers/:id/changes", get(containers_changes))
        .route("/containers/:id/export", get(containers_export))
        .route("/containers/:id/update", post(containers_update))
        .route("/containers/:id/start", post(containers_start))
        .route("/containers/:id/attach", post(containers_attach))
        .route("/containers/:id/stop", post(containers_stop))
        .route("/containers/:id/kill", post(containers_kill))
        .route("/containers/:id/restart", post(containers_restart))
        .route("/containers/:id/pause", post(containers_pause))
        .route("/containers/:id/unpause", post(containers_unpause))
        .route("/containers/:id/rename", post(containers_rename))
        .route("/containers/:id/top", get(containers_top))
        .route("/containers/:id/stats", get(containers_stats))
        .route("/containers/:id/wait", post(containers_wait))
        .route("/containers/:id/resize", post(resize))
        .route("/containers/:id/logs", get(containers_logs))
        .route("/containers/:id/json", get(containers_inspect))
        .route(
            "/containers/:id/archive",
            get(archive_get).put(archive_put).head(archive_head),
        )
        .route("/containers/:id/exec", post(exec_create))
        .route("/exec/:id/start", post(exec_start))
        .route("/exec/:id/resize", post(resize))
        .route("/exec/:id/json", get(exec_inspect))
        .route("/containers/:id", delete(containers_delete))
        .route("/commit", post(commit_container))
        .route("/volumes", get(volumes_list))
        .route("/volumes/create", post(volumes_create))
        .route("/volumes/prune", post(volumes_prune))
        .route("/volumes/:name", get(volume_inspect).delete(volume_delete))
        .route("/networks", get(networks_list))
        .route("/networks/create", post(networks_create))
        .route("/networks/prune", post(networks_prune))
        .route("/networks/:id", get(network_inspect).delete(network_delete))
        .route("/networks/:id/connect", post(network_connect))
        .route("/networks/:id/disconnect", post(network_disconnect))
        .fallback(not_found)
        // Every response carries Docker's negotiation/identity headers so the CLI's version
        // handshake and `docker version`/`info` work without falling back to defaults.
        .layer(axum::middleware::map_response(docker_headers))
        // A Docker daemon ingests large tarball bodies (build contexts, `docker load`, `docker cp`),
        // which exceed axum's 2MB default Bytes-extractor limit -> disable it.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(app)
}
