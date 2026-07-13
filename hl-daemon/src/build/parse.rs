//! Pure argument helpers for `docker build`, factored out of the `images_build` handler so they can be
//! unit-tested in isolation (the handler itself is stateful — it drives the step loop over `App`/rootfs).

use std::collections::HashMap;

/// Decode the `--build-arg` query value — docker sends a JSON object `{"NAME": "value"|null}` — into a
/// `name -> value` map. Args with a `null` value (declared but unset) are dropped, empty/absent/invalid
/// input yields an empty map.
pub(crate) fn parse_build_args(raw: Option<&str>) -> HashMap<String, String> {
    raw.filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<HashMap<String, Option<String>>>(s).ok())
        .map(|m| m.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect())
        .unwrap_or_default()
}

/// Sanitize a raw `-t` tag into a filesystem-safe store directory name: keep `[A-Za-z0-9._-]`, replace
/// everything else (`/`, `:`, …) with `_`. So `-t org/app:v2` maps to a distinct `org_app_v2` dir while a
/// bare `built` keeps a predictable name.
pub(crate) fn safe_dir_name(raw_tag: &str) -> String {
    raw_tag
        .chars()
        .map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_decodes_object_and_drops_nulls() {
        let m = parse_build_args(Some(r#"{"A":"1","B":null,"C":"x y"}"#));
        assert_eq!(m.get("A"), Some(&"1".to_string()));
        assert_eq!(m.get("C"), Some(&"x y".to_string())); // value with a space preserved
        assert!(!m.contains_key("B")); // null -> dropped
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn build_args_empty_absent_and_invalid_are_empty() {
        assert!(parse_build_args(None).is_empty());
        assert!(parse_build_args(Some("")).is_empty());
        assert!(parse_build_args(Some("not json")).is_empty());
    }

    #[test]
    fn safe_dir_name_replaces_separators() {
        assert_eq!(safe_dir_name("org/app:v2"), "org_app_v2");
        assert_eq!(safe_dir_name("built"), "built"); // already safe
        assert_eq!(safe_dir_name("a.b_c-d"), "a.b_c-d"); // ._- kept
        assert_eq!(safe_dir_name("x@y!z"), "x_y_z");
    }
}
