use super::*;

#[derive(Default, Deserialize)]
pub(in super::super) struct CreateQuery {
    name: Option<String>,
}

impl DockerState {
    async fn unpack(&self, value: &str) -> ApiResult<(hl_images::UnpackedImage, hl_images::Metadata)> {
        let images = self.containers.images().map_err(ApiError::container)?;
        let image = self.find_image(value).await?;
        let platform = self.platform.clone();
        tokio::task::spawn_blocking(move || {
            let unpacked = images.unpack(&image, &platform)?;
            let metadata = images.details(&image, &platform)?;
            Ok::<_, hl_images::Error>((unpacked, metadata))
        })
        .await
        .map_err(ApiError::task)?
        .map_err(ApiError::image)
    }
}

pub(in super::super) async fn create(
    State(state): State<DockerState>,
    Query(query): Query<CreateQuery>,
    Json(mut request): Json<CreateContainer>,
) -> ApiResult<(StatusCode, Json<ContainerCreation>)> {
    request.validate_unsupported()?;
    let console_size = crate::api::model::console_size(request.host_config.as_ref().and_then(|host| host.console_size))
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;
    if console_size.is_some() && !request.console.tty {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "HostConfig.ConsoleSize requires Tty=true",
        ));
    }
    let stop_timeout_seconds = request
        .stop_timeout_seconds()
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?;
    let (unpacked, metadata) = state.unpack(&request.image).await?;
    request.merge(metadata)?;
    let network = NetworkPlan::from_request(request.host_config.as_ref(), request.networking_config.as_ref())?;
    let published_ports_discarded = network.mode() == hl_container::NetworkMode::Host
        && request.host_config.as_ref().is_some_and(HostConfig::publishes_ports);
    let healthcheck = request
        .healthcheck
        .map(crate::api::Healthcheck::policy)
        .transpose()
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error))?
        .flatten();
    let network = network.prepare(&state.containers).await?;
    let host = HostSettings::parse_with_sandbox(
        request.host_config.as_ref(),
        &request.exposed_ports,
        request.volumes,
        network.isolated(),
        network.mode(),
        &state.containers,
        state.sandbox,
    )
    .await?;
    let overrides = RuntimeOverrides {
        entrypoint: request.entrypoint,
        command: request.cmd,
        environment: EnvVars::parse(request.env.unwrap_or_default())
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?
            .into_inner(),
        working_directory: request.working_dir.filter(|value| !value.is_empty()),
        user: request.user.filter(|value| !value.is_empty()),
    };
    let name = query.name;
    let labels = request.labels;
    let hostname = request.hostname.filter(|value| !value.is_empty());
    let stop_signal = request
        .stop_signal
        .as_deref()
        .map(str::parse::<DockerSignal>)
        .transpose()
        .map_err(|value| ApiError::new(StatusCode::BAD_REQUEST, format!("invalid stop signal {value:?}")))?
        .map(Signal::from);
    let console = Console {
        stdin: request.console.open,
        terminal: request.console.tty.then(|| console_size.unwrap_or_default()),
    };
    let anonymous = host.anonymous.clone();
    let container = state
        .containers
        .create_image(&unpacked, overrides, move |mut spec| {
            if let Some(name) = name {
                spec = spec.name(name);
            }
            spec.labels = labels;
            if let Some(hostname) = hostname {
                spec = spec.hostname(hostname);
            }
            if let Some(signal) = stop_signal {
                spec = spec.stop_signal(signal);
            }
            if let Some(seconds) = stop_timeout_seconds {
                spec = spec.stop_timeout_seconds(seconds);
            }
            spec.process = spec.process.console(console);
            if let Some(healthcheck) = healthcheck {
                spec = spec.healthcheck(healthcheck);
            }
            host.apply(spec)
        })
        .await;
    let container = match container {
        Ok(container) => container,
        Err(error) => {
            for name in anonymous {
                let _ = state.containers.volumes().remove(&name).await;
            }
            return Err(ApiError::container(error));
        }
    };
    network
        .attach_created(&state.containers, &container.id.to_string())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ContainerCreation::created(
            container.id.to_string(),
            published_ports_discarded,
        )),
    ))
}

impl HostConfig {
    fn publishes_ports(&self) -> bool {
        !self.port_bindings.0.is_empty()
    }

    pub(super) fn validate_unsupported(&self) -> ApiResult<()> {
        if let Some(swappiness) = self.memory_swappiness {
            if !(-1..=100).contains(&swappiness) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "HostConfig.MemorySwappiness must be between 0 and 100, or -1 to inherit",
                ));
            }
            if swappiness != -1 {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "HostConfig.MemorySwappiness tuning is not implemented",
                ));
            }
        }
        if let Some(config) = &self.log_config {
            if !config.kind.is_empty() || !config.config.is_empty() {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "HostConfig.LogConfig is not implemented for configured logging drivers",
                ));
            }
            let fields = crate::api::CompatibilityFields::from(&config.unsupported);
            if let Some(field) = fields.first_meaningful() {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    format!("HostConfig.LogConfig field is not implemented: {field}"),
                ));
            }
        }
        if !self.links.is_empty() {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "HostConfig field is not implemented: Links",
            ));
        }
        let fields = crate::api::CompatibilityFields::from(&self.unsupported);
        let Some(name) = fields.first_meaningful() else {
            return Ok(());
        };
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("HostConfig field is not implemented: {name}"),
        ))
    }
}

impl CreateContainer {
    fn validate_unsupported(&self) -> ApiResult<()> {
        let fields = crate::api::CompatibilityFields::from(&self.unsupported);
        if let Some(name) = fields.first_meaningful() {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                format!("container create field is not implemented: {name}"),
            ));
        }
        if let Some(host) = &self.host_config {
            host.validate_unsupported()?;
        }
        Ok(())
    }

    fn merge(&mut self, metadata: hl_images::Metadata) -> ApiResult<()> {
        self.volumes
            .extend(metadata.volumes.into_iter().map(|path| (path, serde_json::json!({}))));
        self.exposed_ports.0.extend(
            metadata
                .exposed_ports
                .into_iter()
                .filter(|port| port.ends_with("/tcp"))
                .map(|port| (port, serde_json::json!({}))),
        );
        if self.healthcheck.is_none() {
            self.healthcheck = metadata
                .healthcheck
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, error.to_string()))?;
        }
        if self.stop_signal.is_none() {
            self.stop_signal = metadata.stop_signal;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::CreateContainer;

    #[test]
    fn create_preflight_includes_host_capabilities() {
        let inert: CreateContainer = serde_json::from_value(serde_json::json!({
            "Image": "alpine",
            "HostConfig": {"Privileged": false, "CapAdd": []}
        }))
        .unwrap();
        assert!(inert.validate_unsupported().is_ok());

        let meaningful: CreateContainer = serde_json::from_value(serde_json::json!({
            "Image": "alpine",
            "HostConfig": {"Privileged": true}
        }))
        .unwrap();
        assert!(meaningful.validate_unsupported().is_err());
    }

    #[test]
    fn console_size_accepts_wire_shape_before_tty_validation() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"ConsoleSize": null}),
            serde_json::json!({"ConsoleSize": [0, 0]}),
            serde_json::json!({"ConsoleSize": [24, 80]}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            assert!(host.validate_unsupported().is_ok());
        }
        for value in [
            serde_json::json!({"ConsoleSize": [1, 0]}),
            serde_json::json!({"ConsoleSize": [0, 1]}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            assert!(host.validate_unsupported().is_ok());
            assert!(crate::api::model::console_size(host.console_size).is_err());
        }
        for value in [
            serde_json::json!({"ConsoleSize": []}),
            serde_json::json!({"ConsoleSize": [0]}),
            serde_json::json!({"ConsoleSize": [0, 0, 0]}),
            serde_json::json!({"ConsoleSize": "0,0"}),
            serde_json::json!({"ConsoleSize": [-1, 0]}),
        ] {
            assert!(serde_json::from_value::<crate::api::HostConfig>(value).is_err());
        }
    }

    #[test]
    fn log_config_accepts_only_unconfigured_logging() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"LogConfig": null}),
            serde_json::json!({"LogConfig": {}}),
            serde_json::json!({"LogConfig": {"Type": "", "Config": {}}}),
            serde_json::json!({"LogConfig": {"Type": "", "Config": null}}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            assert!(host.validate_unsupported().is_ok());
        }
        for value in [
            serde_json::json!({"LogConfig": {"Type": "json-file", "Config": {}}}),
            serde_json::json!({"LogConfig": {"Type": "", "Config": {"max-size": "10m"}}}),
            serde_json::json!({"LogConfig": {"Future": true}}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            let error = host.validate_unsupported().unwrap_err();
            assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        }
        for value in [
            serde_json::json!({"LogConfig": []}),
            serde_json::json!({"LogConfig": {"Type": 1}}),
            serde_json::json!({"LogConfig": {"Config": []}}),
            serde_json::json!({"LogConfig": {"Config": {"max-size": 10}}}),
        ] {
            assert!(serde_json::from_value::<crate::api::HostConfig>(value).is_err());
        }
    }

    #[test]
    fn memory_swappiness_accepts_only_absent_or_inherited_default() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"MemorySwappiness": null}),
            serde_json::json!({"MemorySwappiness": -1}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            assert!(host.validate_unsupported().is_ok());
        }
        for value in [
            serde_json::json!({"MemorySwappiness": 0}),
            serde_json::json!({"MemorySwappiness": 1}),
            serde_json::json!({"MemorySwappiness": 50}),
            serde_json::json!({"MemorySwappiness": 100}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            let error = host.validate_unsupported().unwrap_err();
            assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        }
        for value in [
            serde_json::json!({"MemorySwappiness": -2}),
            serde_json::json!({"MemorySwappiness": 101}),
            serde_json::json!({"MemorySwappiness": i64::MAX}),
        ] {
            let host: crate::api::HostConfig = serde_json::from_value(value).unwrap();
            let error = host.validate_unsupported().unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
        }
        for value in [
            serde_json::json!({"MemorySwappiness": "-1"}),
            serde_json::json!({"MemorySwappiness": 1.5}),
            serde_json::json!({"MemorySwappiness": []}),
        ] {
            assert!(serde_json::from_value::<crate::api::HostConfig>(value).is_err());
        }
    }
}
