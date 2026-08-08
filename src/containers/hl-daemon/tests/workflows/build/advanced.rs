use super::{Client, CreateContainer, Error, append};

pub(super) async fn advanced(client: &Client) -> Result<(), Error> {
    let arguments = std::collections::BTreeMap::from([("VALUE".into(), "override".into())]);
    client
        .images()
        .build_with(
            std::io::Cursor::new(advanced_context()?),
            "workflow/advanced:test",
            None,
            &arguments,
        )
        .await?;
    let image = client.images().inspect("workflow/advanced:test").await?;
    if image.config.labels.get("org.example.stage").map(String::as_str) != Some("advanced")
        || image.config.user != "nobody"
        || image.config.entrypoint != ["/bin/sh", "-c"]
    {
        return Err(format!("advanced image config mismatch: {:?}", image.config).into());
    }
    let history = client.images().history("workflow/advanced:test").await?;
    if !history
        .iter()
        .any(|entry| entry.created_by == "LABEL org.example.stage=advanced")
        || !history
            .iter()
            .any(|entry| entry.created_by == "RUN echo \"$VALUE\" > /tmp/arg")
    {
        return Err(format!("advanced image history mismatch: {history:?}").into());
    }
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/advanced:test".into(),
                ..CreateContainer::default()
            },
            Some("advanced-contract"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    if status.status_code != 0 || logs.stdout != b"ARG=override USER=65534:65534 FILE=override\n" {
        return Err(format!(
            "advanced built image mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    client.containers().remove(&created.id, false, false).await?;
    Ok(())
}

pub(super) async fn invalid(client: &Client) -> Result<(), Error> {
    for dockerfile in [
        "FROM workflow/alpine:test\nCMD [\"ok\", 1]\n",
        "FROM workflow/alpine:test\nCOPY --chown=user payload /payload\n",
    ] {
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            append(&mut archive, "Dockerfile", dockerfile.as_bytes())?;
            append(&mut archive, "payload", b"x")?;
            archive.finish()?;
        }
        if client
            .images()
            .build(std::io::Cursor::new(bytes), "workflow/invalid:test", None)
            .await
            .is_ok()
        {
            return Err(format!("invalid Dockerfile was accepted: {dockerfile:?}").into());
        }
    }
    Ok(())
}

fn advanced_context() -> Result<Vec<u8>, Error> {
    let dockerfile = b"ARG BASE=workflow/alpine:test\nFROM ${BASE}\nARG VALUE=default\nENV RESULT=${VALUE}\nLABEL org.example.stage=advanced\nSHELL [\"/bin/sh\",\"-eu\",\"-c\"]\nUSER nobody\nRUN echo \"$VALUE\" > /tmp/arg\nENTRYPOINT [\"/bin/sh\",\"-c\"]\nCMD [\"printf 'ARG=%s USER=%s FILE=%s\\n' \\\"$RESULT\\\" \\\"$(id -u):$(id -g)\\\" \\\"$(cat /tmp/arg)\\\"\"]\n";
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        append(&mut archive, "Dockerfile", dockerfile)?;
        archive.finish()?;
    }
    Ok(bytes)
}
