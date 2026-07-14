//! `hl` library surface.
//!
//! The `hl` command itself is the engine-linked binary (`src/main.rs`, built only with the default
//! `cli` feature). This LIBRARY exposes only the light, `hl`-side workspace [`config`] — the bare
//! `hl_ws::Workspace` primitive extended with its feature settings (vpn/cuda/gui/docker_sock/scrollback)
//! plus their persistence — so the GUI can consume it with `default-features = false` and never pull the
//! engine stack.

pub mod config;
