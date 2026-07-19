use super::*;

/// One volume.
#[derive(Debug, Clone, Default)]
pub struct Volume {
    /// Volume name.
    pub name: String,
    /// Volume driver backing it (e.g. `local`).
    pub driver: String,
    /// Host path where the volume's data lives.
    pub mountpoint: String,
    /// Scope the volume is valid in (e.g. `local`, `global`).
    pub scope: String,
    /// User-defined labels, sorted by key.
    pub metadata: Metadata,
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
            metadata: Metadata::new(v.labels, v.options),
            created_at: v.created_at.unwrap_or_default(),
        }
    }
}
