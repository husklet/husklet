//! Cargo-owned build and linkage boundary for Husklet's native C engine.

/// Reports whether this package contains a production engine for the target.
#[must_use]
pub fn supported(target_os: &str, target_arch: &str) -> bool {
    platform::supported(target_os, target_arch)
}

mod platform;
