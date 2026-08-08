use std::io::Read as _;

use super::{Client, CreateContainer, Error, append, support::archive_context};

pub(super) async fn modern_copy(client: &Client) -> Result<(), Error> {
    let context = archive_context(
        b"FROM workflow/alpine:test\nCOPY --exclude=*.tmp --exclude=private dir /filtered/\nCOPY --parents --chown=nobody:nogroup --chmod=0640 /./tree/keep /parents/\nCOPY --parents tree/second /parents/\nCOPY --link=false plain /plain\nCMD sh -c \"cat /filtered/keep /parents/tree/keep /parents/tree/second /plain; test ! -e /filtered/drop.tmp; test ! -e /filtered/private/hidden; stat -c '%u:%g:%a' /parents/tree/keep\"\n",
        &[
            ("dir/keep", b"FILTERED\n"),
            ("dir/drop.tmp", b"bad\n"),
            ("dir/private/hidden", b"bad\n"),
            ("tree/keep", b"PARENT_ONE\n"),
            ("tree/second", b"PARENT_TWO\n"),
            ("plain", b"PLAIN\n"),
        ],
    )?;
    client
        .images()
        .build(std::io::Cursor::new(context), "workflow/modern-copy:test", None)
        .await?;
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/modern-copy:test".into(),
                ..CreateContainer::default()
            },
            Some("modern-copy-contract"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    let expected = b"FILTERED\nPARENT_ONE\nPARENT_TWO\nPLAIN\n65534:65533:640\n";
    if status.status_code != 0 || logs.stdout != expected {
        return Err(format!(
            "modern COPY mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    client.containers().remove(&created.id, false, false).await?;
    Ok(())
}

pub(super) async fn named_ownership(client: &Client) -> Result<(), Error> {
    let context = archive_context(
        b"FROM workflow/alpine:test\nCOPY --chown=nobody:nogroup payload /named\nCOPY --chown=0:nogroup payload /uid-group\nCOPY --chown=nobody:0 payload /name-gid\nCOPY --chown=nobody payload /primary\nCOPY --chown=12:34 payload /numeric\nUSER nobody:nogroup\nCMD sh -c \"id -u; id -g; stat -c '%u:%g' /named /uid-group /name-gid /primary /numeric\"\n",
        &[("payload", b"OWNED\n")],
    )?;
    client
        .images()
        .build(std::io::Cursor::new(context), "workflow/named-ownership:test", None)
        .await?;
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/named-ownership:test".into(),
                ..CreateContainer::default()
            },
            Some("named-ownership-contract"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    let expected = b"65534\n65533\n65534:65533\n0:65533\n65534:0\n65534:65534\n12:34\n";
    if status.status_code != 0 || logs.stdout != expected {
        return Err(format!(
            "named ownership mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    client.containers().remove(&created.id, false, false).await?;
    Ok(())
}

pub(super) async fn external_image_copy(client: &Client) -> Result<(), Error> {
    let source = archive_context(
        b"FROM workflow/alpine:test\nCOPY --chown=12:34 --chmod=0640 payload /external/file\nCOPY --chown=23:45 tree /external/tree\n",
        &[("payload", b"EXTERNAL\n"), ("tree/nested", b"TREE\n")],
    )?;
    client
        .images()
        .build(std::io::Cursor::new(source), "workflow/external-source:test", None)
        .await?;
    let target = archive_context(
        b"FROM workflow/alpine:test AS generated\nRUN echo STAGE > /stage\nFROM workflow/alpine:test\nCOPY --from=generated /stage /stage\nCOPY --from=workflow/external-source:test /external/file /copied/file\nCOPY --from=workflow/external-source:test /external/tree /copied/tree\nCOPY --from=workflow/external-source:test --chmod=0600 /external/file /overridden\nCMD sh -c \"cat /stage /copied/file /copied/tree/nested; stat -c '%u:%g:%a' /copied/file /copied/tree/nested /overridden\"\n",
        &[],
    )?;
    client
        .images()
        .build(std::io::Cursor::new(target), "workflow/external-target:test", None)
        .await?;
    let created = client
        .containers()
        .create(
            &CreateContainer {
                image: "workflow/external-target:test".into(),
                ..CreateContainer::default()
            },
            Some("external-copy-contract"),
        )
        .await?;
    client.containers().start(&created.id).await?;
    let status = client.containers().wait(&created.id).await?;
    let logs = client.containers().logs(&created.id, true, true).await?;
    // Docker COPY creates root-owned entries unless the instruction supplies --chown.
    // Source file modes remain intact, while --chmod explicitly replaces the selected mode.
    let expected = b"STAGE\nEXTERNAL\nTREE\n0:0:640\n0:0:644\n0:0:600\n";
    if status.status_code != 0 || logs.stdout != expected {
        return Err(format!(
            "external image COPY mismatch: status={} stdout={:?} stderr={:?}",
            status.status_code, logs.stdout, logs.stderr
        )
        .into());
    }
    client.containers().remove(&created.id, false, false).await?;
    Ok(())
}

pub(super) async fn ownership(client: &Client) -> Result<(), Error> {
    let mut context = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut context);
        append(
            &mut archive,
            "Dockerfile",
            b"FROM workflow/alpine:test\nCOPY --chown=123:456 payload /owned/payload\nCMD [\"/bin/true\"]\n",
        )?;
        append(&mut archive, "payload", b"owned\n")?;
        archive.finish()?;
    }
    client
        .images()
        .build(std::io::Cursor::new(context), "workflow/ownership:test", None)
        .await?;
    let mut stream = client.images().save(&["workflow/ownership:test"]).await?;
    let mut saved = Vec::new();
    while let Some(chunk) = stream.next_chunk().await? {
        saved.extend_from_slice(&chunk);
    }

    let mut found = false;
    for entry in tar::Archive::new(saved.as_slice()).entries()? {
        let mut entry = entry?;
        if !entry.path()?.to_string_lossy().ends_with(".tar") {
            continue;
        }
        let mut layer = Vec::new();
        entry.read_to_end(&mut layer)?;
        for member in tar::Archive::new(layer.as_slice()).entries()? {
            let member = member?;
            if member.path()?.as_ref() == std::path::Path::new("owned/payload") {
                if member.header().uid()? != 123 || member.header().gid()? != 456 {
                    return Err("COPY --chown archive ownership mismatch".into());
                }
                found = true;
            }
        }
    }
    if !found {
        return Err("COPY --chown output was absent from image layer".into());
    }
    Ok(())
}
