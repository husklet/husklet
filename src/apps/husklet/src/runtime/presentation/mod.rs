//! Native GPU presentation handoff to the compositor.

#[cfg(target_os = "macos")]
pub(super) mod producer;
