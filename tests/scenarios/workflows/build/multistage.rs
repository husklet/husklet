use std::io::Write as _;

use super::{Client, CreateContainer, Error, append};

pub(super) async fn multistage(client: &Client) -> Result<(), Error> {
    let context = multistage_context()?;
    client
        .images()
        .build(std::io::Cursor::new(context.clone()), "workflow/multistage:test", None)
        .await?;
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/multistage:test".into(),
                ..CreateContainer::default()
            },
            Some("multistage-contract"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    if status.status_code != 0
        || logs.stdout != b"STAGE\nNESTED\nONE\nTWO\nARCHIVE\nARCHIVE\nKEEP\nLOG_KEEP\nMULTI_OK\n"
    {
        return Err(format!(
            "multistage image mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    client.containers().remove(&created.id, false, false).await?;

    client
        .images()
        .build_target(
            std::io::Cursor::new(context),
            "workflow/stage-target:test",
            None,
            &std::collections::BTreeMap::new(),
            Some("builder"),
            false,
        )
        .await?;
    let target = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/stage-target:test".into(),
                ..CreateContainer::default()
            },
            Some("target-contract"),
        )
        .await?;
    client.containers().start(&target.id).await?;
    let status = client.containers().wait(&target.id).await?;
    let logs = client.containers().logs(&target.id, true, true).await?;
    if status.status_code != 0 || logs.stdout != b"STAGE\n" {
        return Err("named target stage did not preserve its command/rootfs".into());
    }
    client.containers().remove(&target.id, false, false).await?;
    Ok(())
}

fn multistage_context() -> Result<Vec<u8>, Error> {
    let dockerfile = b"FROM workflow/alpine:test AS builder\nRUN echo STAGE > /stage\nCOPY dir /tree/\nCMD cat /stage\nFROM workflow/alpine:test AS final\nCOPY --from=builder /stage /stage\nCOPY --from=builder /tree /tree\nCOPY one two /multi/\nADD payload.tar /added/\nADD payload.tar.gz /gzip/\nCOPY . /context/\nCMD sh -c \"cat /stage; cat /tree/nested; cat /multi/one; cat /multi/two; cat /added/inside; cat /gzip/inside; cat /context/excluded/keep; cat /context/logs/deep/keep.log; test ! -e /context/ignored.txt; test ! -e /context/drop.secret; test ! -e /context/excluded/drop; test ! -e /context/logs/drop.log; test ! -e /context/Dockerfile; test ! -e /context/.dockerignore; echo MULTI_OK\"\n";
    let mut payload = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut payload);
        append(&mut archive, "inside", b"ARCHIVE\n")?;
        archive.finish()?;
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&payload)?;
    let compressed = encoder.finish()?;
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        append(&mut archive, "Dockerfile", dockerfile)?;
        append(
            &mut archive,
            ".dockerignore",
            b"ignored.txt\n*.secret\nexcluded/\n!excluded/keep\n**/*.log\n!logs/**/keep.log\n",
        )?;
        append(&mut archive, "dir/nested", b"NESTED\n")?;
        append(&mut archive, "one", b"ONE\n")?;
        append(&mut archive, "two", b"TWO\n")?;
        append(&mut archive, "ignored.txt", b"bad\n")?;
        append(&mut archive, "drop.secret", b"bad\n")?;
        append(&mut archive, "excluded/drop", b"bad\n")?;
        append(&mut archive, "excluded/keep", b"KEEP\n")?;
        append(&mut archive, "logs/drop.log", b"bad\n")?;
        append(&mut archive, "logs/deep/keep.log", b"LOG_KEEP\n")?;
        append(&mut archive, "payload.tar", &payload)?;
        append(&mut archive, "payload.tar.gz", &compressed)?;
        archive.finish()?;
    }
    Ok(bytes)
}
