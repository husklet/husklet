//! Pure argument helpers for `docker build`, factored out of the `images_build` handler so they can be
//! unit-tested in isolation (the handler itself is stateful — it drives the step loop over `App`/rootfs).

use std::collections::HashMap;
use std::str::FromStr;

pub(crate) struct BuildArgs(HashMap<String, String>);

impl BuildArgs {
    pub(crate) fn into_map(self) -> HashMap<String, String> {
        self.0
    }
}

impl FromStr for BuildArgs {
    type Err = serde_json::Error;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let values = serde_json::from_str::<HashMap<String, Option<String>>>(source)?;
        Ok(Self(
            values
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value)))
                .collect(),
        ))
    }
}

/// Decode the `--build-arg` query value — docker sends a JSON object `{"NAME": "value"|null}` — into a
/// `name -> value` map. Args with a `null` value (declared but unset) are dropped, empty/absent/invalid
/// input yields an empty map.
/// Sanitize a raw `-t` tag into a filesystem-safe store directory name: keep `[A-Za-z0-9._-]`, replace
/// everything else (`/`, `:`, …) with `_`. So `-t org/app:v2` maps to a distinct `org_app_v2` dir while a
/// bare `built` keeps a predictable name.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_decodes_object_and_drops_nulls() {
        let m = r#"{"A":"1","B":null,"C":"x y"}"#.parse::<BuildArgs>().unwrap().into_map();
        assert_eq!(m.get("A"), Some(&"1".to_string()));
        assert_eq!(m.get("C"), Some(&"x y".to_string())); // value with a space preserved
        assert!(!m.contains_key("B")); // null -> dropped
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn build_args_empty_absent_and_invalid_are_empty() {
        assert!("".parse::<BuildArgs>().is_err());
        assert!("not json".parse::<BuildArgs>().is_err());
    }

    #[test]
    fn safe_dir_name_replaces_separators() {
        assert_eq!(
            hl_images::Key::from_name("org/app:v2").as_str(),
            "org%2Fapp%3Av2"
        );
    }
}
