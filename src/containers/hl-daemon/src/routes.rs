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
use crate::http::DockerHttp;
use crate::images::*;
use crate::model::App;
use crate::model::Container;
use crate::networks::*;
use crate::system::*;
use crate::volumes::*;

/// Build the fully wired daemon router over the shared [`App`] state.
pub(crate) struct Routes;
impl Routes {
    pub(crate) fn new(app: App) -> Router {
        Router::new()
            .route("/_ping", get(|| async { "OK" }))
            .route("/version", get(version))
            .route("/info", get(System::info))
            .route("/events", get(events::Events::stream))
            .route("/system/df", get(System::df))
            .route("/system/prune", post(System::prune))
            .route("/plugins", get(plugins_list))
            .route("/auth", post(System::auth))
            .route("/distribution/:name/json", get(ImageApi::distribution))
            .route("/images/json", get(ImageApi::list))
            .route("/images/create", post(images_create))
            .route("/images/get", get(Images::save))
            .route("/images/load", post(ImageLoad::handle))
            .route("/images/search", get(ImageApi::image_search))
            .route("/images/prune", post(ImageApi::prune))
            .route("/images/:name/json", get(ImageApi::inspect))
            .route("/images/:name/history", get(ImageApi::history))
            .route("/images/:name/push", post(image_push))
            .route("/build", post(images_build))
            .route("/build/prune", post(build_prune))
            .route("/images/:name/tag", post(image_tag))
            .route("/images/:name", delete(image_delete))
            .route("/containers/json", get(Containers::list))
            .route("/containers/create", post(containers_create))
            .route("/containers/prune", post(Containers::prune))
            .route("/containers/:id/changes", get(Containers::changes))
            .route("/containers/:id/export", get(Containers::export))
            .route("/containers/:id/update", post(containers_update))
            .route("/containers/:id/start", post(Containers::start))
            .route("/containers/:id/attach", post(containers_attach))
            .route("/containers/:id/stop", post(containers_stop))
            .route("/containers/:id/kill", post(containers_kill))
            .route("/containers/:id/restart", post(containers_restart))
            .route("/containers/:id/pause", post(Containers::pause))
            .route("/containers/:id/unpause", post(Containers::unpause))
            .route("/containers/:id/rename", post(containers_rename))
            .route("/containers/:id/top", get(Containers::top))
            .route("/containers/:id/stats", get(containers_stats))
            .route("/containers/:id/wait", post(containers_wait))
            .route("/containers/:id/resize", post(resize))
            .route("/containers/:id/logs", get(containers_logs))
            .route("/containers/:id/json", get(Containers::inspect))
            .route(
                "/containers/:id/archive",
                get(archive_get).put(archive_put).head(archive_head),
            )
            .route("/containers/:id/exec", post(exec_create))
            .route("/exec/:id/start", post(exec_start))
            .route("/exec/:id/resize", post(resize))
            .route("/exec/:id/json", get(Execs::inspect))
            .route("/containers/:id", delete(containers_delete))
            .route("/commit", post(Container::commit))
            .route("/volumes", get(Volumes::list))
            .route("/volumes/create", post(Volumes::create))
            .route("/volumes/prune", post(Volumes::prune))
            .route(
                "/volumes/:name",
                get(Volumes::inspect).delete(Volumes::delete),
            )
            .route("/networks", get(Networks::list))
            .route("/networks/create", post(Networks::create))
            .route("/networks/prune", post(Networks::prune))
            .route(
                "/networks/:id",
                get(Networks::inspect).delete(Networks::delete),
            )
            .route("/networks/:id/connect", post(network_connect))
            .route("/networks/:id/disconnect", post(network_disconnect))
            .fallback(DockerHttp::not_found)
            // Every response carries Docker's negotiation/identity headers so the CLI's version
            // handshake and `docker version`/`info` work without falling back to defaults.
            .layer(axum::middleware::map_response(DockerHttp::headers))
            // A Docker daemon ingests large tarball bodies (build contexts, `docker load`, `docker cp`),
            // which exceed axum's 2MB default Bytes-extractor limit -> disable it.
            .layer(axum::extract::DefaultBodyLimit::disable())
            .with_state(app)
    }
}
