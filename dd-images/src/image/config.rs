//! Small helpers over image references and OCI config blobs (store paths, ref parsing, config arrays).

use crate::registry::ImageRef;
use serde_json::Value;

/// The store path component for a reference: its canonical form with `/` and `:` flattened to `_`.
pub fn safe_name(r: &ImageRef) -> String {
    r.canonical().replace(['/', ':'], "_")
}

/// Parse `from_image` into an [`ImageRef`], overriding the tag with `tag` when non-empty.
pub fn image_ref(from_image: &str, tag: &str) -> ImageRef {
    let mut r = ImageRef::parse(from_image);
    if !tag.is_empty() {
        r.tag = tag.to_string();
    }
    r
}

/// A string array at `config.config.<key>` of an OCI config blob, flattened to `Vec<String>`.
pub fn config_strs(config: &Value, key: &str) -> Vec<String> {
    config["config"][key]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
