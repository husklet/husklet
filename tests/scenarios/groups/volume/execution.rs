use hl_client::{
    Client,
    model::{CreateContainer, HostConfig},
};

use super::{Error, IMAGE};

pub(super) fn request(command: &str, binds: Vec<String>) -> CreateContainer {
    CreateContainer {
        image: IMAGE.into(),
        cmd: Some(vec!["/bin/sh".into(), "-c".into(), command.into()]),
        host_config: Some(HostConfig {
            binds,
            ..HostConfig::default()
        }),
        ..CreateContainer::default()
    }
}

pub(super) async fn execute(client: &Client, name: &str, command: &str, binds: Vec<String>) -> Result<Vec<u8>, Error> {
    let created = client.containers().create(&request(command, binds), Some(name)).await?;
    client.containers().start(&created.id).await?;
    let status = match client.containers().wait(&created.id).await {
        Ok(status) => status,
        Err(error) => {
            let _ = client.containers().remove(&created.id, true, false).await;
            return Err(error.into());
        }
    };
    let logs = client.containers().logs(&created.id, true, true).await?;
    client.containers().remove(&created.id, false, false).await?;
    if status.status_code != 0 {
        return Err(format!(
            "{name} exited {}: {}",
            status.status_code,
            String::from_utf8_lossy(&logs.stderr)
        )
        .into());
    }
    Ok(logs.stdout)
}

pub(super) fn pass(ok: bool, id: &str) -> Result<(), Error> {
    if !ok {
        return Err(id.into());
    }
    println!("PASS {id}");
    Ok(())
}
