use super::*;
use bollard::models::ImageSummary;

/// One entry of `GET /images/json`.
#[derive(Debug, Clone, Default)]
pub struct Image {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub architecture: String,
    pub size: i64,
    /// Unix creation time (seconds) — for newest-first sorting.
    pub created: i64,
}

impl From<ImageSummary> for Image {
    fn from(i: ImageSummary) -> Self {
        // bollard's ImageSummary has no Architecture field (the dd daemon emits it as an extra,
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
