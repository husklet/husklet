use super::*;

/// One volume.
#[derive(Debug, Clone, Default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub scope: String,
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
    /// ISO-8601 creation time (sorts chronologically as a string) — for newest-first sorting.
    pub created_at: String,
}

impl From<bollard::models::Volume> for Volume {
    fn from(v: bollard::models::Volume) -> Self {
        Volume {
            name: v.name,
            driver: v.driver,
            mountpoint: v.mountpoint,
            scope: v.scope.map(|s| s.to_string()).unwrap_or_default(),
            labels: sorted_pairs(v.labels),
            options: sorted_pairs(v.options),
            created_at: v.created_at.unwrap_or_default(),
        }
    }
}
