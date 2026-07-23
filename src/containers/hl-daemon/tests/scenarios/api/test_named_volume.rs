//! Named volume persistence across service restart and containers.

use crate::api::support::require;
use hl_container::{Config, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use std::path::Path;

pub(crate) async fn run(work: &Path, rootfs: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::new(work.join("volume-state"));
    let containers = Containers::builder(config.clone()).build().await?;
    containers
        .volumes()
        .create(hl_container::VolumeSpec::new("shared"))
        .await?;
    containers
        .create(
            ContainerSpec::from_directory(
                rootfs,
                Process::new("/bin/sh").args(["-c", "printf persistent > /data/value"]),
            )
            .name("volume-writer")
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            })
            .mount(hl_container::Mount::volume_read_write("shared", "/data")),
        )
        .await?;
    containers.start("volume-writer").await?;
    require(
        containers.wait("volume-writer").await? == ExitStatus::Code(0),
        "named-volume writer failed",
    )?;
    drop(containers);

    let containers = Containers::builder(config).build().await?;
    containers
        .create(
            ContainerSpec::from_directory(
                rootfs,
                Process::new("/bin/sh")
                    .args(["-c", "read value < /data/value; printf '%s\\n' \"$value\""]),
            )
            .name("volume-reader")
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            })
            .mount(hl_container::Mount::volume_read_only("shared", "/data")),
        )
        .await?;
    containers.start("volume-reader").await?;
    require(
        containers.wait("volume-reader").await? == ExitStatus::Code(0)
            && containers.logs("volume-reader").await?.stdout == b"persistent\n",
        "named volume did not survive service restart and independent containers",
    )?;
    Ok(())
}
