#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerSummary {
    pub(crate) names: Vec<String>,
    pub(crate) image: String,
    status: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ImageSummary {
    pub(crate) repo_tags: Vec<String>,
    id: String,
    pub(crate) size: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeSummary {
    pub(crate) name: String,
    driver: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumesResponse {
    pub(crate) volumes: Vec<VolumeSummary>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworkSummary {
    name: String,
    driver: String,
    pub(crate) scope: String,
}

pub(crate) struct WorkspaceResources<'a> {
    client: crate::host::transport::LocalHttp<'a>,
}

pub(crate) struct ResourceRows {
    pub(crate) containers: Vec<Vec<String>>,
    pub(crate) images: Vec<Vec<String>>,
    pub(crate) volumes: Vec<Vec<String>>,
    pub(crate) networks: Vec<Vec<String>>,
}

impl<'a> WorkspaceResources<'a> {
    pub(crate) fn new(socket: &'a std::path::Path) -> Self {
        Self {
            client: crate::host::transport::LocalHttp::new(socket),
        }
    }

    pub(crate) fn read(&self) -> std::io::Result<ResourceRows> {
        Ok(ResourceRows {
            containers: self.containers()?,
            images: self.images()?,
            volumes: self.volumes()?,
            networks: self.networks()?,
        })
    }

    fn containers(&self) -> std::io::Result<Vec<Vec<String>>> {
        Ok(self
            .client
            .get::<Vec<ContainerSummary>>("/containers/json?all=1")?
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
            .collect())
    }

    fn images(&self) -> std::io::Result<Vec<Vec<String>>> {
        Ok(self
            .client
            .get::<Vec<ImageSummary>>("/images/json")?
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
            .collect())
    }

    fn volumes(&self) -> std::io::Result<Vec<Vec<String>>> {
        Ok(self
            .client
            .get::<VolumesResponse>("/volumes")?
            .volumes
            .into_iter()
            .map(|volume| vec![volume.name, volume.driver])
            .collect())
    }

    fn networks(&self) -> std::io::Result<Vec<Vec<String>>> {
        Ok(self
            .client
            .get::<Vec<NetworkSummary>>("/networks")?
            .into_iter()
            .map(|network| vec![network.name, network.driver, network.scope])
            .collect())
    }
}
