//! dd-daemon — a Docker-Engine-API daemon backed by the **dd** VM-less JIT runtime.
//!
//! The real `docker` CLI (and the `dd-app` GUI) talk to this over a Unix socket; container
//! *execution* is delegated to the JIT binaries built by the `ddjit` crate (one per guest
//! architecture). The daemon detects each image's architecture from its ELF and picks the
//! matching JIT, then launches it via the typed [`hl_jit::SpawnConfig`] contract — no VM.
//!
//!   cargo run --release -p hl-daemon            # build.rs builds the JITs first
//!   DOCKER_HOST=unix://$PWD/dd.sock docker run -p 8080:80 -m 256m alpine echo hi
//!
//! Containers, volumes and networks are persisted to `HL_STATE` (default `~/.dd/state.json`) so
//! they survive daemon restarts. Images are re-discovered from `HL_IMAGES` each startup.
//!
//! Env: HL_IMAGES (image dirs; default "./images"), HL_DOCKER_SOCK (listen socket),
//!      HL_STATE (state file), HL_VOLUMES (named-volume root).

use hl_jit::Guest;
use std::sync::Arc;
use tokio::sync::Mutex;

use hl_images::registry;

mod api;
mod archive;
mod build;
mod containers;
mod events;
mod http;
mod images;
mod model;
mod networks;
mod prelude;
mod routes;
mod runtime;
mod system;
#[cfg(test)]
mod test_support;
mod util;
mod volumes;

use crate::http::strip_api_version;
use crate::model::*;
use crate::networks::default_networks;
use crate::util::*;

/// Read-only bundled starter-image dirs to discover ALONGSIDE the writable `images_dir`: the app
/// bundle's `Resources/images`, a sibling of this daemon binary. We discover (not copy) them so an app
/// update always serves the current starter images and `~/.dd` never needs a manual refresh. Empty in a
/// dev/test tree (no such sibling exists next to the binary), so it can't perturb the matrix.
fn bundled_image_dirs(images_dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("images");
            if p.is_dir() && p.to_string_lossy() != images_dir {
                out.push(p.to_string_lossy().into_owned());
            }
        }
    }
    out
}

#[tokio::main]
async fn main() {
    let images_dir = std::env::var("HL_IMAGES").unwrap_or_else(|_| "./images".into());
    let sock = std::env::var("HL_DOCKER_SOCK").unwrap_or_else(|_| "./dd.sock".into());
    let state_path = std::env::var("HL_STATE")
        .unwrap_or_else(|_| hl_home().join("state.json").to_string_lossy().into_owned());
    let volumes_dir = std::env::var("HL_VOLUMES")
        .unwrap_or_else(|_| hl_home().join("volumes").to_string_lossy().into_owned());
    // Only unlink the configured socket path when it is actually a (stale) unix socket — an env-only
    // override must NOT blindly delete an arbitrary regular file/dir a misconfigured HL_DOCKER_SOCK points
    // at. A non-socket is left in place (the bind below then fails loudly instead of eating user data).
    match std::fs::symlink_metadata(&sock) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_socket() {
                let _ = std::fs::remove_file(&sock);
            } else {
                eprintln!(
                    "[dd-daemon] refusing to remove non-socket HL_DOCKER_SOCK path {sock:?} \
                     (not a stale socket); bind will fail if it is occupied"
                );
            }
        }
        Err(_) => {} // path doesn't exist yet — nothing to unlink
    }
    let _ = std::fs::create_dir_all(&volumes_dir);

    let mut inner = Inner::default();
    // Discover the writable user image store (HL_IMAGES = ~/.dd/images) PLUS any read-only bundled
    // starter images shipped inside the app (Resources/images, beside this binary). Serving the bundled
    // set straight from the bundle -- instead of copying it into ~/.dd -- means an app update always
    // carries the current starter images and nothing in ~/.dd ever needs refreshing. User pulls win on
    // a name clash.
    let mut imgs = discover_images(&images_dir);
    for d in bundled_image_dirs(&images_dir) {
        for img in discover_images(&d) {
            if !imgs.iter().any(|i| i.name == img.name) {
                imgs.push(img);
            }
        }
    }
    inner.images = imgs;
    load_state(&mut inner, &state_path);
    if inner.networks.is_empty() {
        inner.networks = default_networks();
    }
    eprintln!(
        "[dd-daemon] images={} -> {} image(s): {}",
        images_dir,
        inner.images.len(),
        inner
            .images
            .iter()
            .map(|i| format!("{}({})", i.name, i.arch.target()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    eprintln!(
        "[dd-daemon] state={state_path} -> {} container(s), {} volume(s), {} network(s)",
        inner.containers.len(),
        inner.volumes.len(),
        inner.networks.len()
    );
    for g in Guest::ALL {
        eprintln!(
            "[dd-daemon] JIT {}: {}",
            g.target(),
            if hl_jit::available(g) {
                "ready"
            } else {
                "NOT BUILT"
            }
        );
    }
    let app = App {
        inner: Arc::new(Mutex::new(inner)),
        state_path,
        volumes_dir,
        images_dir,
        events: events::new_bus(),
    };

    let router = routes::router(app);

    let listener = tokio::net::UnixListener::bind(&sock).expect("bind unix socket");
    eprintln!("[dd-daemon] listening on unix://{sock}");
    let mut make = router.into_make_service();
    loop {
        let (socket, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let svc = tower::Service::call(&mut make, &socket).await.unwrap();
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(socket);
            let hsvc = hyper::service::service_fn(
                move |mut req: hyper::Request<hyper::body::Incoming>| {
                    strip_api_version(&mut req);
                    if std::env::var("HL_DEBUG").is_ok() {
                        eprintln!("[req] {} {}", req.method(), req.uri().path());
                    }
                    tower::ServiceExt::oneshot(svc.clone(), req)
                },
            );
            // serve_connection_with_upgrades: required for the attach/exec hijack (HTTP Upgrade: tcp).
            let _ =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection_with_upgrades(io, hsvc)
                    .await;
        });
    }
}
