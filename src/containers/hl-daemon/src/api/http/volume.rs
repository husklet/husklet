use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use hl_container::VolumeSpec;
use serde::Deserialize;
use std::collections::BTreeMap;

use super::{ApiError, ApiResult, DockerState};
use crate::api::{Volume, VolumeCreate, VolumeList, VolumePrune};

#[derive(Default, Deserialize)]
pub(super) struct ListQuery {
    pub(super) filters: Option<String>,
}

#[hl_design::adapter]
pub(super) async fn list(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<VolumeList>> {
    let filters = Filters::parse(query.filters, &["dangling", "driver", "label", "name"])?;
    let service = state.containers.volumes();
    let mut volumes = Vec::new();
    for volume in service.list().await.map_err(ApiError::container)? {
        let references = service
            .references(&volume.name)
            .await
            .map_err(ApiError::container)?;
        if filters.matches(&volume, references) {
            volumes.push(Volume::from(volume));
        }
    }
    Ok(Json(VolumeList {
        volumes,
        warnings: Vec::new(),
    }))
}

#[hl_design::adapter]
pub(super) async fn create(
    State(state): State<DockerState>,
    Json(request): Json<VolumeCreate>,
) -> ApiResult<(StatusCode, Json<Volume>)> {
    request.validate()?;
    if request.cluster_volume_spec.is_some() {
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "cluster volumes are not supported by the local driver",
        ));
    }
    if !request.driver.is_empty() && request.driver != "local" {
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("volume driver {:?} is not implemented", request.driver),
        ));
    }
    let backing = LocalOptions::parse(&request.driver_opts)?;
    if request.name.is_empty() && backing.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "local bind volume options require an explicit volume name",
        ));
    }
    let volumes = state.containers.volumes();
    let volume = if request.name.is_empty() {
        volumes.create_anonymous(request.labels).await
    } else {
        let mut spec = VolumeSpec::new(request.name);
        spec.labels = request.labels;
        spec.options = request.driver_opts;
        if let Some(backing) = backing {
            spec = spec.bind(backing.device, backing.read_only);
        }
        volumes.create(spec).await
    }
    .map_err(ApiError::container)?;
    let mut attributes = volume.labels.clone();
    attributes.insert("name".into(), volume.name.clone());
    attributes.insert("driver".into(), "local".into());
    state
        .events
        .object("volume", "create", &volume.name, attributes);
    Ok((StatusCode::CREATED, Json(Volume::from(volume))))
}

impl VolumeCreate {
    fn validate(&self) -> ApiResult<()> {
        let Some(name) =
            crate::api::CompatibilityFields::from(&self.unsupported).first_meaningful()
        else {
            return Ok(());
        };
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("volume create field {name:?} is not implemented"),
        ))
    }
}

#[derive(Debug)]
pub(super) struct LocalOptions {
    pub(super) device: String,
    pub(super) read_only: bool,
}

impl LocalOptions {
    pub(super) fn parse(options: &BTreeMap<String, String>) -> ApiResult<Option<Self>> {
        if options.is_empty() {
            return Ok(None);
        }
        if let Some(name) = options
            .keys()
            .find(|name| !matches!(name.as_str(), "type" | "o" | "device"))
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("local volume option {name:?} is not implemented"),
            ));
        }
        if options.get("type").map(String::as_str) != Some("none") {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "only local volume type=none is implemented",
            ));
        }
        let modes = options.get("o").ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "local bind volume requires option o=bind",
            )
        })?;
        let mut bind = false;
        let mut read_only = false;
        let mut access = None;
        for mode in modes.split(',') {
            match mode {
                "bind" if !bind => bind = true,
                "ro" if access.is_none() => {
                    access = Some("ro");
                    read_only = true;
                }
                "rw" if access.is_none() => access = Some("rw"),
                "" => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "local volume option o contains an empty mode",
                    ))
                }
                "bind" | "ro" | "rw" => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        format!("duplicate local volume mode {mode:?}"),
                    ))
                }
                _ => {
                    return Err(ApiError::new(
                        StatusCode::NOT_IMPLEMENTED,
                        format!("local volume mode {mode:?} is not implemented"),
                    ))
                }
            }
        }
        if !bind {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "local volume option o must include bind",
            ));
        }
        let device = options
            .get("device")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "local bind volume requires an absolute device directory",
                )
            })?;
        if !std::path::Path::new(device).is_absolute() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "local bind volume device must be absolute",
            ));
        }
        Ok(Some(Self {
            device: device.clone(),
            read_only,
        }))
    }
}

#[hl_design::adapter]
pub(super) async fn inspect(
    State(state): State<DockerState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Volume>> {
    state
        .containers
        .volumes()
        .inspect(&name)
        .await
        .map(Volume::from)
        .map(Json)
        .map_err(ApiError::container)
}

#[derive(Default, Deserialize)]
pub(super) struct RemoveQuery {
    #[allow(dead_code)]
    force: bool,
}

#[hl_design::adapter]
pub(super) async fn remove(
    State(state): State<DockerState>,
    Path(name): Path<String>,
    Query(_query): Query<RemoveQuery>,
) -> ApiResult<StatusCode> {
    let volume = state
        .containers
        .volumes()
        .remove(&name)
        .await
        .map_err(ApiError::container)?;
    let mut attributes = volume.labels;
    attributes.insert("name".into(), volume.name.clone());
    attributes.insert("driver".into(), "local".into());
    state
        .events
        .object("volume", "destroy", volume.name, attributes);
    Ok(StatusCode::NO_CONTENT)
}

#[hl_design::adapter]
pub(super) async fn prune(
    State(state): State<DockerState>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<VolumePrune>> {
    let filters = Filters::parse(query.filters, &["all", "label", "label!"])?;
    let all = filters.enabled("all");
    let service = state.containers.volumes();
    let mut removed = Vec::new();
    let mut space_reclaimed = 0_u64;
    for volume in service.list().await.map_err(ApiError::container)? {
        let references = service
            .references(&volume.name)
            .await
            .map_err(ApiError::container)?;
        if references != 0
            || (!all && volume.kind != hl_container::VolumeKind::Anonymous)
            || !filters.matches(&volume, references)
        {
            continue;
        }
        let size = service
            .size(&volume.name)
            .await
            .map_err(ApiError::container)?;
        match service.remove(&volume.name).await {
            Ok(volume) => {
                space_reclaimed = space_reclaimed.saturating_add(size);
                let mut attributes = volume.labels;
                attributes.insert("name".into(), volume.name.clone());
                attributes.insert("driver".into(), "local".into());
                attributes.insert("reclaimed".into(), "true".into());
                state
                    .events
                    .object("volume", "destroy", &volume.name, attributes);
                removed.push(volume.name);
            }
            Err(hl_container::Error::VolumeInUse(_)) => {}
            Err(error) => return Err(ApiError::container(error)),
        }
    }
    Ok(Json(VolumePrune {
        volumes_deleted: removed,
        space_reclaimed,
    }))
}

#[derive(Default)]
struct Filters(BTreeMap<String, Vec<String>>);

impl Filters {
    fn parse(raw: Option<String>, allowed: &[&str]) -> ApiResult<Self> {
        let Some(raw) = raw.filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        let values: BTreeMap<String, serde_json::Value> = serde_json::from_str(&raw)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        let mut filters = BTreeMap::new();
        for (name, values) in values {
            if !allowed.contains(&name.as_str()) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unsupported volume filter {name:?}"),
                ));
            }
            let values = match values {
                serde_json::Value::Array(values) => values
                    .into_iter()
                    .map(|value| {
                        value.as_str().map(str::to_owned).ok_or_else(|| {
                            ApiError::new(StatusCode::BAD_REQUEST, "filter values must be strings")
                        })
                    })
                    .collect::<ApiResult<Vec<_>>>()?,
                serde_json::Value::Object(values) => values
                    .into_iter()
                    .filter_map(|(value, enabled)| (enabled == true).then_some(value))
                    .collect(),
                _ => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "filter values must be arrays or objects",
                    ))
                }
            };
            filters.insert(name, values);
        }
        Ok(Self(filters))
    }

    fn enabled(&self, name: &str) -> bool {
        self.0.get(name).is_some_and(|values| {
            values
                .iter()
                .any(|value| matches!(value.as_str(), "1" | "true"))
        })
    }

    fn matches(&self, volume: &hl_container::Volume, references: usize) -> bool {
        self.matches_values("driver", |value| value == "local")
            && self.matches_values("name", |value| volume.name.contains(value))
            && self.matches_values("dangling", |value| match value.as_str() {
                "1" | "true" => references == 0,
                "0" | "false" => references != 0,
                _ => false,
            })
            && self.matches_values("label", |value| Self::label(&volume.labels, value))
            && self.matches_all("label!", |value| !Self::label(&volume.labels, value))
    }

    fn matches_values(&self, name: &str, predicate: impl Fn(&String) -> bool) -> bool {
        self.0
            .get(name)
            .is_none_or(|values| values.iter().any(predicate))
    }

    fn matches_all(&self, name: &str, predicate: impl Fn(&String) -> bool) -> bool {
        self.0
            .get(name)
            .is_none_or(|values| values.iter().all(predicate))
    }

    fn label(labels: &BTreeMap<String, String>, filter: &str) -> bool {
        filter.split_once('=').map_or_else(
            || labels.contains_key(filter),
            |(name, value)| labels.get(name).is_some_and(|actual| actual == value),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalOptions, VolumeCreate};
    use axum::http::StatusCode;
    use std::collections::BTreeMap;

    #[test]
    fn local_options_accept_only_explicit_bind_directory_contract() {
        let valid = BTreeMap::from([
            ("device".into(), "/host/data".into()),
            ("o".into(), "bind,ro".into()),
            ("type".into(), "none".into()),
        ]);
        let parsed = LocalOptions::parse(&valid).unwrap().unwrap();
        assert_eq!(parsed.device, "/host/data");
        assert!(parsed.read_only);

        let writable = BTreeMap::from([
            ("device".into(), "/host/data".into()),
            ("o".into(), "bind,rw".into()),
            ("type".into(), "none".into()),
        ]);
        assert!(!LocalOptions::parse(&writable).unwrap().unwrap().read_only);

        for invalid in [
            BTreeMap::from([("type".into(), "ext4".into())]),
            BTreeMap::from([
                ("type".into(), "none".into()),
                ("o".into(), "bind,ro,rw".into()),
                ("device".into(), "/host/data".into()),
            ]),
            BTreeMap::from([
                ("type".into(), "none".into()),
                ("o".into(), "bind".into()),
                ("device".into(), "relative".into()),
            ]),
        ] {
            assert!(LocalOptions::parse(&invalid).is_err());
        }
        assert_eq!(
            LocalOptions::parse(&BTreeMap::from([("type".into(), "ext4".into())]))
                .unwrap_err()
                .status,
            StatusCode::NOT_IMPLEMENTED
        );
    }

    #[test]
    fn volume_create_preserves_and_rejects_meaningful_unknown_fields() {
        let harmless: VolumeCreate = serde_json::from_value(serde_json::json!({
            "Name": "cache",
            "FutureOption": false
        }))
        .unwrap();
        harmless.validate().unwrap();

        let meaningful: VolumeCreate = serde_json::from_value(serde_json::json!({
            "Name": "cache",
            "FutureOption": {"mode": "shared"}
        }))
        .unwrap();
        let error = meaningful.validate().unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        assert!(format!("{error:?}").contains("FutureOption"));
    }
}
