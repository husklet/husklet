//! System and misc endpoints.

use crate::{Client, DiskUsage, Result, SystemInfo};

impl Client {
    /// `GET /_ping` — returns `Ok(())` when the daemon is alive.
    pub async fn ping(&self) -> Result<()> {
        self.docker()?.ping().await.map(|_| ())
    }

    /// `GET /version` + `GET /info` flattened into one [`SystemInfo`] for the System view.
    pub async fn system(&self) -> Result<SystemInfo> {
        let d = self.docker()?;
        let v = d.version().await.unwrap_or_default();
        let i = d.info().await?;
        Ok(SystemInfo {
            version: v.version.unwrap_or_default(),
            api_version: v.api_version.unwrap_or_default(),
            os: v.os.unwrap_or_default(),
            arch: v.arch.unwrap_or_default(),
            kernel: i.kernel_version.unwrap_or_default(),
            driver: i.driver.unwrap_or_default(),
            root_dir: i.docker_root_dir.unwrap_or_default(),
            server_version: i.server_version.unwrap_or_default(),
            ncpu: i.ncpu.unwrap_or_default(),
            mem_total: i.mem_total.unwrap_or_default(),
            containers: i.containers.unwrap_or_default(),
            running: i.containers_running.unwrap_or_default(),
            paused: i.containers_paused.unwrap_or_default(),
            stopped: i.containers_stopped.unwrap_or_default(),
            images: i.images.unwrap_or_default(),
        })
    }

    /// `GET /system/df` summarized into a [`DiskUsage`].
    pub async fn disk_usage(&self) -> Result<DiskUsage> {
        let r = self
            .docker()?
            .df(None::<bollard::query_parameters::DataUsageOptions>)
            .await?;
        Ok(DiskUsage {
            layers_size: r
                .image_usage
                .as_ref()
                .and_then(|u| u.total_size)
                .unwrap_or_default(),
            images: r
                .image_usage
                .as_ref()
                .and_then(|u| u.total_count)
                .unwrap_or_default(),
            containers: r
                .container_usage
                .as_ref()
                .and_then(|u| u.total_count)
                .unwrap_or_default(),
            volumes: r
                .volume_usage
                .as_ref()
                .and_then(|u| u.total_count)
                .unwrap_or_default(),
        })
    }
}
