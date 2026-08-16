use axum::Router;
use axum::routing::{any, delete, get, post};
use hl_container::Containers;
use hl_images::Platform;
use hl_images::remote::Source;
use std::sync::Arc;

mod build;
mod console;
mod container;
pub(in crate::api) use container::DockerSignal;
mod error;
mod event;
mod exec;
mod image;
mod network;
mod observe;
mod push;
mod query;
mod system;
mod version;
mod volume;

use crate::daemon::Release;
use crate::events::Events;
use error::{ApiError, ApiResult};

#[derive(Clone)]
pub(super) struct DockerState {
    pub(super) containers: Containers,
    pub(super) platform: Platform,
    pub(super) source: Arc<dyn Source>,
    pub(super) events: Events,
    pub(super) builds: crate::builder::Builds,
    pub(super) release: Release,
    pub(super) sampler: Arc<dyn crate::ProcessSampler>,
    pub(super) sandbox: hl_container::Sandbox,
}

pub(crate) fn router(
    containers: Containers,
    platform: Platform,
    source: Arc<dyn Source>,
    events: Events,
    release: Release,
    sampler: Arc<dyn crate::ProcessSampler>,
    sandbox: hl_container::Sandbox,
) -> Router {
    let state = DockerState {
        containers,
        platform,
        source,
        events,
        builds: crate::builder::Builds::default(),
        release,
        sampler,
        sandbox,
    };
    let api = Router::new()
        .route("/_ping", get(system::ping).head(system::ping_head))
        .route("/version", get(system::version))
        .route("/build", post(build::create))
        .route("/build/prune", post(build::prune))
        .route("/containers/json", get(container::list))
        .route("/events", get(event::get))
        .route("/containers/create", post(container::create))
        .route("/containers/prune", post(container::prune))
        .route("/containers/:id/json", get(container::inspect))
        .route("/containers/:id/changes", get(container::changes))
        .route("/containers/:id/update", post(container::update))
        .route("/containers/:id/export", get(container::export))
        .route(
            "/containers/:id/archive",
            get(container::archive).head(container::stat).put(container::extract),
        )
        .route("/containers/:id/logs", get(container::logs))
        .route("/containers/:id/top", get(observe::top))
        .route("/containers/:id/stats", get(observe::stats))
        .route("/containers/:id/attach", post(container::attach))
        .route("/containers/:id/attach/ws", get(container::websocket_attach))
        .route("/containers/:id/start", post(container::start))
        .route("/containers/:id/resize", post(container::resize))
        .route("/containers/:id/kill", post(container::kill))
        .route("/containers/:id/rename", post(container::rename))
        .route("/containers/:id/wait", post(container::wait))
        .route("/containers/:id", delete(container::remove))
        .route("/containers/:id/pause", post(container::pause))
        .route("/containers/:id/unpause", post(container::unpause))
        .route("/containers/:id/checkpoint", post(container::checkpoint))
        .route("/containers/:id/exec", post(exec::create))
        .route("/exec/:id/start", post(exec::start))
        .route("/exec/:id/resize", post(exec::resize))
        .route("/exec/:id/kill", post(exec::signal))
        .route("/exec/:id/json", get(exec::inspect))
        .route("/exec/:id/wait", post(exec::wait))
        .route("/exec/:id", delete(exec::remove))
        .route("/info", get(system::info))
        .route("/plugins", get(system::plugins))
        .route("/auth", post(system::auth))
        .route("/system/df", get(system::disk))
        .route("/system/prune", post(system::prune))
        .route("/images/json", get(image::list))
        .route("/commit", post(image::commit))
        .route("/images/search", get(image::search))
        .route("/distribution/*path", any(image::named_distribution))
        .route("/images/load", post(image::load))
        .route("/images/create", post(image::pull))
        .route("/images/prune", post(image::prune))
        .route("/images/get", get(image::save))
        .route("/images/*path", any(image::named))
        .route("/networks", get(network::list))
        .route("/networks/create", post(network::create))
        .route("/networks/prune", post(network::prune))
        .route("/networks/:id", get(network::inspect).delete(network::remove))
        .route("/networks/:id/connect", post(network::connect))
        .route("/networks/:id/disconnect", post(network::disconnect))
        .route("/volumes", get(volume::list))
        .route("/volumes/create", post(volume::create))
        .route("/volumes/prune", post(volume::prune))
        .route("/volumes/:name", get(volume::inspect).delete(volume::remove));
    let legacy_api = api
        .clone()
        .route("/containers/:id/stop", post(container::legacy_stop))
        .route("/containers/:id/restart", post(container::legacy_restart));
    let current_api = api
        .route("/containers/:id/stop", post(container::stop))
        .route("/containers/:id/restart", post(container::restart));
    let mut router = Router::new().merge(current_api.clone());
    for minor in 24..=41 {
        router = router.nest(&format!("/v1.{minor}"), legacy_api.clone());
    }
    for minor in 42..=43 {
        router = router.nest(&format!("/v1.{minor}"), current_api.clone());
    }
    router.fallback(version::fallback).with_state(state)
}
