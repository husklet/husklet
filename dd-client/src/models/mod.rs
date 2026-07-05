//! View models for the dd daemon, built from [`bollard`]'s Docker-API responses. These are the
//! shapes the dd GUI and CLI render; each has a `From<bollard::...>` conversion and the small
//! display helpers (`short_id`, `name`, `ports_str`, …) the UI relies on.

mod container;
mod image;
mod network;
mod system;
mod volume;

pub use container::*;
pub use image::*;
pub use network::*;
pub use system::*;
pub use volume::*;

/// docker-style short id (first 12 hex chars).
pub fn short(id: &str) -> String {
    id.trim_start_matches("sha256:").chars().take(12).collect()
}

/// Sort a `{k: v}` map into stable `(k, v)` display pairs.
pub(super) fn sorted_pairs(m: std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = m.into_iter().collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── short() / short_id() ─────────────────────────────────────────────────
    #[test]
    fn short_truncates_64_hex_to_12() {
        let id = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(short(id), "0123456789ab");
        assert_eq!(short(id).len(), 12);
    }

    #[test]
    fn short_strips_sha256_prefix_then_truncates() {
        // the `sha256:` prefix is dropped BEFORE the 12-char take.
        let id = "sha256:0123456789abcdef0123456789abcdef";
        assert_eq!(short(id), "0123456789ab");
    }

    #[test]
    fn short_passes_through_ids_shorter_than_12() {
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
    }
}
