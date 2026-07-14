//! `dd-daemon` — a Docker-Engine-API polyfill (a thin transport layer) on top of the `dd-jit` runtime.
//!
//! The daemon **binary** (`src/main.rs`) serves the Docker HTTP API and translates each request into
//! `hl_jit` calls — it owns no container-runtime logic of its own. The Docker-Engine-API *client* the
//! dd CLI and GUI use now lives in its own `dd-client` crate; the daemon no longer re-exports it.
