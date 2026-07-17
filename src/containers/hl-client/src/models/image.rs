use super::*;
use bollard::models::ImageSummary;

/// One entry of `GET /images/json`.
#[derive(Debug, Clone, Default)]
pub struct Image {
    /// Full image id (`sha256:…`).
    pub id: String,
    /// Repository tags naming this image (e.g. `alpine:latest`).
    pub repo_tags: Vec<String>,
    /// CPU architecture the image targets (blank; not carried by the list endpoint).
    pub architecture: String,
    /// Total on-disk size of the image, in bytes.
    pub size: i64,
    /// Unix creation time (seconds) — for newest-first sorting.
    pub created: i64,
}

impl From<ImageSummary> for Image {
    fn from(i: ImageSummary) -> Self {
        // bollard's ImageSummary has no Architecture field (the hl daemon emits it as an extra,
        // which bollard drops). The UI only displays it, so leave it blank.
        Image {
            id: i.id,
            repo_tags: i.repo_tags,
            architecture: String::new(),
            size: i.size,
            created: i.created,
        }
    }
}

impl Image {
    /// First repo tag (e.g. `alpine:latest`), or the short id.
    pub fn name(&self) -> String {
        self.repo_tags
            .first()
            .cloned()
            .unwrap_or_else(|| short(&self.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_prefers_first_repo_tag() {
        let img = Image {
            repo_tags: vec!["alpine:latest".into(), "alpine:3.20".into()],
            ..Default::default()
        };
        assert_eq!(img.name(), "alpine:latest");
    }

    #[test]
    fn name_falls_back_to_short_id_when_untagged() {
        // An untagged image (`<none>:<none>`) carries no repo_tags -> short id.
        let img = Image {
            id: "sha256:0123456789abcdeffedcba9876543210".into(),
            ..Default::default()
        };
        assert_eq!(img.name(), "0123456789ab");
    }
}
