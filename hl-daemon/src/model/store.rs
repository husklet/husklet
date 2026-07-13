use super::*;

/// The serializable slice of [`Inner`] written to `DD_STATE`.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Persisted {
    pub(crate) containers: Vec<Container>,
    pub(crate) volumes: Vec<Vol>,
    pub(crate) networks: Vec<Net>,
}
