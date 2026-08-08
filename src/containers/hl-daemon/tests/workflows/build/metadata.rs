use super::{Client, CreateContainer, Error, append};

pub(super) async fn metadata(client: &Client) -> Result<(), Error> {
    client
        .images()
        .build(
            std::io::Cursor::new(metadata_context(
                "FROM workflow/alpine:test\nONBUILD COPY --chmod=0750 trigger /trigger\nONBUILD ENV CHILD=onbuild\nEXPOSE 8080 9090/udp\nVOLUME [\"/data\"]\nHEALTHCHECK --interval=5s --retries=3 CMD test -f /trigger\nSTOPSIGNAL SIGQUIT\nCMD [\"/bin/true\"]\n",
                None,
            )?),
            "workflow/metadata:test",
            None,
        )
        .await?;
    let parent = client.images().inspect("workflow/metadata:test").await?;
    if parent.config.onbuild.len() != 2
        || !parent.config.exposed_ports.contains_key("8080/tcp")
        || !parent.config.exposed_ports.contains_key("9090/udp")
        || !parent.config.volumes.contains_key("/data")
        || parent.config.stop_signal.as_deref() != Some("SIGQUIT")
        || parent
            .config
            .healthcheck
            .as_ref()
            .and_then(|value| value.get("Retries"))
            != Some(&serde_json::json!(3))
    {
        return Err(format!("built metadata was not preserved: {:?}", parent.config).into());
    }
    client
        .images()
        .build(
            std::io::Cursor::new(metadata_context(
                "FROM workflow/metadata:test\nCMD [\"/bin/sh\",\"-c\",\"printf '%s\\n' \\\"$CHILD\\\"; stat -c '%a' /trigger; cat /trigger\"]\n",
                Some(b"TRIGGER\n"),
            )?),
            "workflow/metadata-child:test",
            None,
        )
        .await?;
    let child = client.images().inspect("workflow/metadata-child:test").await?;
    if !child.config.onbuild.is_empty() {
        return Err("consumed ONBUILD triggers leaked into child metadata".into());
    }
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/metadata-child:test".into(),
                ..CreateContainer::default()
            },
            Some("metadata-contract"),
        )
        .await?;
    let inspected = client.containers().inspect(&created.id).await?;
    if !inspected.config.exposed_ports.contains_key("8080/tcp")
        || !inspected
            .metadata
            .mounts
            .iter()
            .any(|mount| mount.destination == "/data")
        || inspected.config.stop_signal != "SIGQUIT"
    {
        return Err("image ports/volumes were not applied at container creation".into());
    }
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    if status.status_code != 0 || logs.stdout != b"onbuild\n750\nTRIGGER\n" {
        return Err(format!("ONBUILD execution mismatch: {status:?} {logs:?}").into());
    }
    client.containers().remove(&created.id, false, true).await?;
    let explicit = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/metadata-child:test".into(),
                stop_signal: Some("SIGTERM".into()),
                ..CreateContainer::default()
            },
            Some("metadata-signal-override"),
        )
        .await?;
    if client.containers().inspect(&explicit.id).await?.config.stop_signal != "SIGTERM" {
        return Err("explicit stop signal did not override the image default".into());
    }
    client.containers().remove(&explicit.id, false, true).await?;
    Ok(())
}

fn metadata_context(dockerfile: &str, trigger: Option<&[u8]>) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut archive = tar::Builder::new(&mut bytes);
    append(&mut archive, "Dockerfile", dockerfile.as_bytes())?;
    if let Some(trigger) = trigger {
        append(&mut archive, "trigger", trigger)?;
    }
    archive.finish()?;
    drop(archive);
    Ok(bytes)
}
