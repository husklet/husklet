use super::{
    ApiError, ApiResult, BTreeMap, Deserialize, DockerState, Field, Fields, ImagePrune,
    ImageSummary, Json, Query, State, StatusCode,
};

#[derive(Default, Deserialize)]
pub(in super::super) struct ListQuery {
    pub(super) all: Option<String>,
    pub(super) digests: Option<String>,
    #[serde(rename = "shared-size")]
    pub(super) shared_size: Option<String>,
    pub(super) filters: Option<String>,
    #[serde(flatten)]
    pub(super) unsupported: BTreeMap<String, String>,
}

impl ListQuery {
    pub(super) fn selection(&self) -> ApiResult<(ImageSelection, bool)> {
        Fields::from(&self.unsupported).reject("image list")?;
        let _all = Field::new("all", self.all.as_deref()).boolean()?;
        let _digests = Field::new("digests", self.digests.as_deref()).boolean()?;
        let shared_size = Field::new("shared-size", self.shared_size.as_deref()).boolean()?;
        Ok((ImageSelection::parse(self.filters.as_deref())?, shared_size))
    }
}

#[derive(Debug, Default)]
pub(super) struct ImageSelection {
    values: BTreeMap<String, Vec<String>>,
}

impl ImageSelection {
    fn parse(raw: Option<&str>) -> ApiResult<Self> {
        let Some(raw) = raw.filter(|value| Field::meaningful(value)) else {
            return Ok(Self::default());
        };
        let values: BTreeMap<String, Vec<String>> = serde_json::from_str(raw).map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid image list filters: {error}"),
            )
        })?;
        let unsupported = values
            .keys()
            .filter(|name| !matches!(name.as_str(), "reference" | "dangling" | "label"))
            .cloned()
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("unsupported image list filters: {}", unsupported.join(", ")),
            ));
        }
        for value in values.get("dangling").into_iter().flatten() {
            Field::new("dangling", Some(value)).boolean()?;
        }
        Ok(Self { values })
    }

    fn matches(&self, image: &ImageSummary) -> bool {
        self.values.iter().all(|(name, values)| {
            values.is_empty()
                || values.iter().any(|value| match name.as_str() {
                    "reference" => image
                        .repo_tags
                        .iter()
                        .any(|reference| Self::wildcard(value, reference)),
                    "dangling" => {
                        let dangling = image.repo_tags.is_empty();
                        Field::new("dangling", Some(value))
                            .boolean()
                            .is_ok_and(|expected| dangling == expected)
                    }
                    "label" => Self::label(&image.labels, value),
                    _ => false,
                })
        })
    }

    pub(super) fn wildcard(pattern: &str, value: &str) -> bool {
        let pattern = pattern.as_bytes();
        let value = value.as_bytes();
        let mut previous = vec![false; value.len() + 1];
        previous[0] = true;
        for &token in pattern {
            let mut current = vec![false; value.len() + 1];
            if token == b'*' {
                current[0] = previous[0];
            }
            for index in 1..=value.len() {
                current[index] = match token {
                    b'*' => previous[index] || current[index - 1],
                    b'?' => previous[index - 1],
                    byte => previous[index - 1] && byte == value[index - 1],
                };
            }
            previous = current;
        }
        previous[value.len()]
    }

    fn label(labels: &BTreeMap<String, String>, filter: &str) -> bool {
        filter.split_once('=').map_or_else(
            || labels.contains_key(filter),
            |(name, value)| labels.get(name).is_some_and(|actual| actual == value),
        )
    }
}

#[hl_design::adapter]
pub(in super::super) async fn list(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<ImageSummary>>> {
    let (selection, shared_size) = query.selection()?;
    // This store has no unnamed intermediate-image records, and summaries always carry digest
    // identities, so both Docker flags are already satisfied by the same projection.
    Ok(Json(
        state
            .image_summaries_with_shared_size(shared_size)
            .await?
            .into_iter()
            .filter(|image| selection.matches(image))
            .collect(),
    ))
}

#[derive(Default, Deserialize)]
pub(in super::super) struct PruneQuery {
    filters: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

#[hl_design::adapter]
pub(in super::super) async fn prune(
    State(state): State<DockerState>,
    Query(query): Query<PruneQuery>,
) -> ApiResult<Json<ImagePrune>> {
    Fields::from(&query.unsupported).reject("image prune")?;
    if let Some(filters) = query.filters.filter(|value| !value.is_empty()) {
        let filters: BTreeMap<String, Vec<String>> =
            serde_json::from_str(&filters).map_err(|error| {
                ApiError::new(StatusCode::BAD_REQUEST, format!("invalid filters: {error}"))
            })?;
        for (name, values) in filters {
            if name != "dangling" || values.iter().any(|value| value != "true" && value != "1") {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    format!("unsupported image prune filter {name:?}"),
                ));
            }
        }
    }
    let images = state.containers.images().map_err(ApiError::container)?;
    let report = tokio::task::spawn_blocking(move || images.gc())
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?;
    Ok(Json(ImagePrune {
        images_deleted: Vec::new(),
        space_reclaimed: i64::try_from(report.content_bytes_removed).unwrap_or(i64::MAX),
    }))
}

pub(in super::super) enum Prune {
    All,
    Selected {
        values: BTreeMap<String, Vec<String>>,
        until: Option<u64>,
    },
}

impl Prune {
    pub(in super::super) fn parse(filters: Option<&str>) -> ApiResult<Self> {
        let Some(raw) = filters.filter(|value| !value.is_empty()) else {
            return Ok(Self::All);
        };
        let values: BTreeMap<String, Vec<String>> = serde_json::from_str(raw)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        let until = values
            .get("until")
            .map(|values| {
                let [value] = values.as_slice() else {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "until requires one value",
                    ));
                };
                value
                    .parse::<crate::api::filter::PruneCutoff>()
                    .map(|until| until.milliseconds())
                    .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))
            })
            .transpose()?;
        Ok(Self::Selected { values, until })
    }

    pub(in super::super) async fn execute(self, state: &DockerState) -> ApiResult<ImagePrune> {
        let images = state.containers.images().map_err(ApiError::container)?;
        let report = match self {
            Self::All => tokio::task::spawn_blocking(move || images.gc())
                .await
                .map_err(ApiError::task)?
                .map_err(ApiError::image)?,
            Self::Selected { values, until } => {
                let containers = state.containers.list().await.map_err(ApiError::container)?;
                let referenced = containers
                    .iter()
                    .filter_map(|container| container.spec.image.as_ref().map(ToString::to_string))
                    .collect::<std::collections::BTreeSet<_>>();
                let graphs = images.graphs().map_err(ApiError::image)?;
                let selected = graphs
                    .into_iter()
                    .filter(hl_images::Graph::filterable)
                    .filter(|graph| graph.names.is_empty() || graph.build_cache)
                    .filter(|graph| {
                        !referenced.contains(&graph.target.digest().to_string())
                            && graph.names.iter().all(|name| !referenced.contains(name))
                    })
                    .filter(|graph| {
                        until.is_none_or(|until| graph.created_at_ms.is_some_and(|v| v < until))
                    })
                    .filter(|graph| {
                        let labels = graph.labels.as_ref().expect("filterable graph has labels");
                        values.get("label").is_none_or(|filters| {
                            filters
                                .iter()
                                .any(|value| ImageSelection::label(labels, value))
                        }) && values.get("label!").is_none_or(|filters| {
                            filters
                                .iter()
                                .all(|value| !ImageSelection::label(labels, value))
                        })
                    })
                    .map(|graph| graph.target.digest().to_string())
                    .collect();
                tokio::task::spawn_blocking(move || images.prune_graphs(&selected))
                    .await
                    .map_err(ApiError::task)?
                    .map_err(ApiError::image)?
            }
        };
        Ok(ImagePrune {
            images_deleted: Vec::new(),
            space_reclaimed: i64::try_from(report.content_bytes_removed).unwrap_or(i64::MAX),
        })
    }
}
