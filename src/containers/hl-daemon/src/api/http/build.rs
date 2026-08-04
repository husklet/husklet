use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hl_images::Reference;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, fmt::Write as _};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::{ApiError, ApiResult, DockerState};
use crate::builder::{BaseImages, BuildNetwork, Builder};

#[derive(Deserialize)]
pub(super) struct BuildQuery {
    #[serde(default = "BuildQuery::default_tag")]
    t: String,
    #[serde(default = "BuildQuery::default_dockerfile")]
    dockerfile: String,
    buildargs: Option<String>,
    target: Option<String>,
    #[serde(default)]
    nocache: bool,
    #[serde(default = "default_remove")]
    rm: bool,
    #[serde(default)]
    forcerm: bool,
    pull: Option<String>,
    #[serde(rename = "q")]
    quiet: Option<String>,
    platform: Option<String>,
    networkmode: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

impl BuildQuery {
    fn default_tag() -> String {
        "built:latest".into()
    }

    fn default_dockerfile() -> String {
        "Dockerfile".into()
    }

    fn arguments(&self) -> ApiResult<BTreeMap<String, String>> {
        let Some(value) = self.buildargs.as_deref() else {
            return Ok(BTreeMap::new());
        };
        let values =
            serde_json::from_str::<BTreeMap<String, Option<String>>>(value).map_err(|error| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("invalid buildargs: {error}"),
                )
            })?;
        Ok(values
            .into_iter()
            .filter_map(|(name, value)| value.map(|value| (name, value)))
            .collect())
    }

    fn validate(&self) -> ApiResult<()> {
        if !self.rm && !self.forcerm {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "rm=false is not implemented; intermediate containers are always removed",
            ));
        }
        self.network()?;
        if let Some((name, _)) = self
            .unsupported
            .iter()
            .find(|(_, value)| !matches!(value.as_str(), "" | "0" | "false" | "[]" | "{}"))
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("build option {name} is not implemented"),
            ));
        }
        Ok(())
    }

    fn base_images(&self) -> BaseImages {
        if self
            .pull
            .as_deref()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        {
            BaseImages::Pull
        } else {
            BaseImages::Local
        }
    }

    fn output(&self) -> BuildOutput {
        if self
            .quiet
            .as_deref()
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        {
            BuildOutput::Quiet
        } else {
            BuildOutput::Progress
        }
    }

    fn network(&self) -> ApiResult<BuildNetwork> {
        match self.networkmode.as_deref().unwrap_or_default() {
            "" | "default" | "bridge" => Ok(BuildNetwork::Default),
            "none" => Ok(BuildNetwork::None),
            "host" => Ok(BuildNetwork::Host),
            value if value.starts_with("container:") => Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("build networkmode {value:?} is not implemented"),
            )),
            value => Ok(BuildNetwork::Named(value.to_owned())),
        }
    }

    fn target_platform(&self, supported: &hl_images::Platform) -> ApiResult<hl_images::Platform> {
        let Some(value) = self.platform.as_deref().filter(|value| !value.is_empty()) else {
            return Ok(supported.clone());
        };
        let requested = value.parse::<hl_images::Platform>().map_err(|error| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("invalid platform: {error}"),
            )
        })?;
        if requested != *supported {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "requested platform {value} cannot execute on this engine platform {}/{}{}",
                    supported.os,
                    supported.architecture,
                    supported
                        .variant
                        .as_ref()
                        .map_or_else(String::new, |variant| format!("/{variant}"))
                ),
            ));
        }
        Ok(requested)
    }
}

pub(super) async fn create(
    State(state): State<DockerState>,
    Query(query): Query<BuildQuery>,
    body: Body,
) -> ApiResult<Response> {
    query.validate()?;
    query.target_platform(&state.platform)?;
    let name: Reference = query.t.parse().map_err(ApiError::image_request)?;
    let arguments = query.arguments()?;
    let mut stream = body.into_data_stream();
    let mut context = tokio::fs::File::from_std(tempfile::tempfile().map_err(ApiError::io)?);
    let mut received = 0_u64;
    let mut digest = Sha256::new();
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        let chunk =
            chunk.map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        received = received.saturating_add(chunk.len() as u64);
        if received > 8 * 1024 * 1024 * 1024 {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "build context exceeds 8 GiB",
            ));
        }
        digest.update(&chunk);
        context.write_all(&chunk).await.map_err(ApiError::io)?;
    }
    context
        .seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(ApiError::io)?;
    digest.update(query.dockerfile.as_bytes());
    digest.update(serde_json::to_vec(&arguments).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid buildargs: {error}"),
        )
    })?);
    digest.update(query.target.as_deref().unwrap_or_default().as_bytes());
    digest.update(query.platform.as_deref().unwrap_or_default().as_bytes());
    let cache = (!query.nocache).then(|| {
        digest
            .finalize()
            .iter()
            .fold(String::with_capacity(64), |mut output, byte| {
                write!(output, "{byte:02x}").expect("writing to a String cannot fail");
                output
            })
    });
    let _guard = match &cache {
        Some(key) => Some(state.builds.lock(key).await),
        None => None,
    };
    let events = state.events.clone();
    let images = state.containers.images().map_err(ApiError::container)?;
    let platform = state.platform.clone();
    let image = Builder::new(state.containers, state.platform, state.source)
        .network(query.network()?)
        .build(
            context.into_std().await,
            crate::builder::BuildRequest {
                dockerfile: &query.dockerfile,
                name,
                arguments: &arguments,
                target: query.target.as_deref(),
                cache,
                base_images: query.base_images(),
            },
        )
        .await
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
    let selected = image.clone();
    let id = tokio::task::spawn_blocking(move || images.image_id(&selected, &platform))
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)?
        .to_string();
    events.image("build", &id, image.name.to_string());
    let body = query.output().success(&id);
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuildOutput {
    Progress,
    Quiet,
}

impl BuildOutput {
    fn success(self, id: &str) -> String {
        let mut lines = Vec::with_capacity(2);
        if self == Self::Progress {
            lines.push(serde_json::json!({"stream": "Successfully built\n"}));
        }
        lines.push(serde_json::json!({"aux": {"ID": id}}));
        let mut body = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        body.push('\n');
        body
    }
}

#[hl_design::adapter]
pub(super) async fn prune(
    State(state): State<DockerState>,
) -> ApiResult<Json<crate::api::BuildPrune>> {
    let images = state.containers.images().map_err(ApiError::container)?;
    let report = tokio::task::spawn_blocking(move || {
        for image in images.list()? {
            if image.name.repository().starts_with("hl-build-cache/") {
                images.remove(&image.name)?;
            }
        }
        let gc = images.gc()?;
        Ok::<_, hl_images::Error>(crate::api::BuildPrune {
            space_reclaimed: gc.content_bytes_removed,
        })
    })
    .await
    .map_err(ApiError::task)?
    .map_err(ApiError::image)?;
    Ok(Json(report))
}

const fn default_remove() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{BuildOutput, BuildQuery};
    use crate::builder::{BaseImages, BuildNetwork};
    use std::collections::BTreeMap;

    fn query() -> BuildQuery {
        BuildQuery {
            t: BuildQuery::default_tag(),
            dockerfile: BuildQuery::default_dockerfile(),
            buildargs: None,
            target: None,
            nocache: false,
            rm: true,
            forcerm: false,
            pull: None,
            quiet: None,
            platform: None,
            networkmode: None,
            unsupported: BTreeMap::new(),
        }
    }

    #[test]
    fn meaningful_unsupported_build_options_fail_explicitly() {
        assert!(query().validate().is_ok());
        let mut value = query();
        value.pull = Some("true".into());
        assert!(value.validate().is_ok());
        assert_eq!(value.base_images(), BaseImages::Pull);
        value.pull = Some("false".into());
        assert_eq!(value.base_images(), BaseImages::Local);
        value.quiet = Some("true".into());
        assert!(value.validate().is_ok());
        assert_eq!(value.output(), BuildOutput::Quiet);
        for (mode, expected) in [
            ("default", BuildNetwork::Default),
            ("bridge", BuildNetwork::Default),
            ("none", BuildNetwork::None),
            ("host", BuildNetwork::Host),
        ] {
            value.networkmode = Some(mode.into());
            assert_eq!(value.network().unwrap(), expected);
            assert!(value.validate().is_ok());
        }
        value.networkmode = Some("named-network".into());
        assert_eq!(
            value.network().unwrap(),
            BuildNetwork::Named("named-network".into())
        );
        value.networkmode = Some("container:parent".into());
        assert_eq!(
            value.validate().unwrap_err().status,
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
        let mut value = query();
        value.platform = Some("linux/amd64".into());
        assert!(value.validate().is_ok());
        assert!(value
            .target_platform(&hl_images::Platform::linux_arm64())
            .is_err());
        value.platform = Some("linux/arm64".into());
        assert_eq!(
            value
                .target_platform(&hl_images::Platform::linux_arm64())
                .unwrap(),
            hl_images::Platform::linux_arm64()
        );
        let mut value = query();
        value.unsupported.insert("memory".into(), "0".into());
        assert!(value.validate().is_ok());
        let mut value = query();
        value.unsupported.insert("memory".into(), "1024".into());
        assert_eq!(
            value.validate().unwrap_err().status,
            axum::http::StatusCode::NOT_IMPLEMENTED
        );
    }

    #[test]
    fn build_arguments_decode_strings_and_drop_null_values() {
        let mut value = query();
        value.buildargs = Some(r#"{"KEEP":"value","DROP":null}"#.into());
        assert_eq!(
            value.arguments().unwrap(),
            BTreeMap::from([("KEEP".into(), "value".into())])
        );
    }

    #[test]
    fn absent_build_arguments_are_empty_and_malformed_values_fail() {
        assert!(query().arguments().unwrap().is_empty());
        for invalid in ["", "not json", r#"{"ARG":7}"#] {
            let mut value = query();
            value.buildargs = Some(invalid.into());
            assert!(value.arguments().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn build_success_stream_uses_lowercase_stream_and_nested_uppercase_id() {
        let lines = BuildOutput::Progress
            .success("sha256:identity")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            lines[0],
            serde_json::json!({"stream":"Successfully built\n"})
        );
        assert_eq!(
            lines[1],
            serde_json::json!({"aux":{"ID":"sha256:identity"}})
        );

        let quiet = BuildOutput::Quiet
            .success("sha256:identity")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(quiet, [serde_json::json!({"aux":{"ID":"sha256:identity"}})]);
    }
}
