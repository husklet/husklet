//! Fresh-pull glibc dynamic-loader smoke workflow.

use hl_container::{ContainerSpec, Containers, ExitStatus, Guest, Isolation, Process, Sandbox};
use hl_images::{
    Images, Platform, Reference,
    remote::{Auth, Registry},
};
use tempfile::TempDir;

type Error = Box<dyn std::error::Error>;

struct Case {
    image: &'static str,
    platform: Platform,
    marker: &'static str,
}

pub(super) async fn run(containers: &Containers) -> Result<(), Error> {
    let cache = TempDir::new()?;
    let images = Images::open(cache.path())?;
    let registry = Registry::new(Auth::Anonymous);
    for case in cases() {
        let reference: Reference = case.image.parse()?;
        let image = images.pull(&registry, reference, &case.platform).await?;
        let unpacked = images.unpack(&image, &case.platform)?;
        let root = images.rootfs(&unpacked)?;
        let view = images.roots().open(&root)?;
        let name = format!(
            "smoke-{}-{}",
            case.platform.architecture,
            case.image.replace([':', '/'], "-")
        );
        let outcome = async {
            containers
                .create(
                    ContainerSpec::from_directory(view.path(), Process::new("/bin/echo").args([case.marker]))
                        .guest(Guest::for_platform(&case.platform)?)
                        .name(&name)
                        .isolation(Isolation {
                            sandbox: Sandbox::Disabled,
                            network_isolated: true,
                            ..Isolation::default()
                        }),
                )
                .await?;
            containers.start(&name).await?;
            let status = containers.wait(&name).await?;
            let logs = containers.logs(&name).await?;
            if status != ExitStatus::Code(0) || logs.stdout != format!("{}\n", case.marker).as_bytes() {
                return Err::<(), Error>(
                    format!(
                        "{}/{}: status={status:?} stdout={:?} stderr={:?}",
                        case.platform.architecture,
                        case.image,
                        String::from_utf8_lossy(&logs.stdout),
                        String::from_utf8_lossy(&logs.stderr)
                    )
                    .into(),
                );
            }
            Ok(())
        }
        .await;
        let cleanup = cleanup(containers, &name, &images, &root).await;
        combine(outcome, cleanup)?;
        println!("PASS smoke-realimage {}/{}", case.platform.architecture, case.image);
    }
    if !containers.list().await?.is_empty() {
        return Err("smoke workflow leaked container records".into());
    }
    Ok(())
}

fn combine(outcome: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; cleanup: {cleanup}").into()),
    }
}

async fn cleanup(
    containers: &Containers,
    name: &str,
    images: &Images,
    root: &hl_images::rootfs::Reference,
) -> Result<(), Error> {
    let remove = if containers.inspect(name).await.is_ok() {
        containers.remove_force(name).await.map(|_| ()).map_err(Error::from)
    } else {
        Ok(())
    };
    let release = images.roots().release(root).map_err(Error::from);
    let closed = images.roots().open(root).is_err();
    remove?;
    release?;
    if !closed {
        return Err("smoke rootfs lease remained open after release".into());
    }
    Ok(())
}

fn cases() -> [Case; 3] {
    [
        Case {
            image: "ubuntu:latest",
            platform: Platform::linux_arm64(),
            marker: "SMOKE-UBUNTU-ARM64",
        },
        Case {
            image: "debian:latest",
            platform: Platform::linux_arm64(),
            marker: "SMOKE-DEBIAN-ARM64",
        },
        Case {
            image: "debian:latest",
            platform: Platform::linux_amd64(),
            marker: "SMOKE-DEBIAN-AMD64",
        },
    ]
}
