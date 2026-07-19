#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct ContainerSummary {
    pub(crate) names: Vec<String>,
    pub(crate) image: String,
    status: String,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct ImageSummary {
    pub(crate) repo_tags: Vec<String>,
    id: String,
    pub(crate) size: i64,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct VolumeSummary {
    pub(crate) name: String,
    driver: String,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct VolumesResponse {
    pub(crate) volumes: Vec<VolumeSummary>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub(crate) struct NetworkSummary {
    name: String,
    driver: String,
    pub(crate) scope: String,
}

pub(crate) struct WorkspaceResources<'a> {
    client: crate::host::transport::LocalHttp<'a>,
}

impl<'a> WorkspaceResources<'a> {
    pub(crate) fn new(socket: &'a std::path::Path) -> Self {
        Self {
            client: crate::host::transport::LocalHttp::new(socket),
        }
    }

    pub(crate) fn containers(&self) -> Vec<Vec<String>> {
        self.client
            .get::<Vec<ContainerSummary>>("/containers/json?all=1")
            .unwrap_or_default()
            .into_iter()
            .map(|container| {
                let name = container
                    .names
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .to_string();
                vec![name, container.image, container.status]
            })
            .collect()
    }

    pub(crate) fn images(&self) -> Vec<Vec<String>> {
        self.client
            .get::<Vec<ImageSummary>>("/images/json")
            .unwrap_or_default()
            .into_iter()
            .map(|image| {
                let repository = image
                    .repo_tags
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "<none>".to_string());
                let id = image
                    .id
                    .trim_start_matches("sha256:")
                    .chars()
                    .take(12)
                    .collect();
                let size = format!("{} MB", image.size / 1_000_000);
                vec![repository, id, size]
            })
            .collect()
    }

    pub(crate) fn volumes(&self) -> Vec<Vec<String>> {
        self.client
            .get::<VolumesResponse>("/volumes")
            .unwrap_or_default()
            .volumes
            .into_iter()
            .map(|volume| vec![volume.name, volume.driver])
            .collect()
    }

    pub(crate) fn networks(&self) -> Vec<Vec<String>> {
        self.client
            .get::<Vec<NetworkSummary>>("/networks")
            .unwrap_or_default()
            .into_iter()
            .map(|network| vec![network.name, network.driver, network.scope])
            .collect()
    }
}
