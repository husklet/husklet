//! `dd-daemon` — a Docker-Engine-API polyfill (a thin transport layer) on top of the `dd-jit` runtime.
//!
//! The daemon **binary** (`src/main.rs`) serves the Docker HTTP API and translates each request into
//! `dd_jit` calls — it owns no container-runtime logic of its own. This **library** target additionally
//! exposes the Docker-Engine-API *client* (absorbed from the former `dd-client` crate) so the dd CLI and
//! GUI depend only on `dd-daemon`.
pub mod client;
