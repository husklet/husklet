use super::{Client, Error, support::archive_context};

pub(super) async fn run_mounts(client: &Client) -> Result<(), Error> {
    let first = archive_context(
        b"FROM workflow/alpine:test AS source\nRUN mkdir /stage-data && echo STAGE_BIND > /stage-data/value\nFROM workflow/alpine:test\nRUN --mount=type=bind,source=context-data,target=/context-data,ro --mount=type=bind,from=source,source=/stage-data,target=/stage-data,readonly cat /context-data/value /stage-data/value > /result && ! touch /context-data/blocked\nRUN --mount=type=cache,id=workflow-persist,target=/cache,sharing=shared test ! -e /cache/value && echo PERSISTED > /cache/value\nRUN --mount=type=cache,id=workflow-persist,target=/cache,sharing=shared cat /cache/value >> /result\nCMD sh -c \"cat /result; test ! -e /cache/value\"\n",
        &[("context-data/value", b"CONTEXT_BIND\n")],
    )?;
    client
        .images()
        .build_target(
            std::io::Cursor::new(first),
            "workflow/run-mounts:first",
            None,
            &std::collections::BTreeMap::new(),
            None,
            true,
        )
        .await?;

    let second = archive_context(
        b"FROM workflow/alpine:test\nRUN --mount=type=cache,id=workflow-persist,target=/cache,sharing=shared test \"$(cat /cache/value)\" = PERSISTED && echo REUSED > /verified\nCMD cat /verified\n",
        &[],
    )?;
    client
        .images()
        .build_target(
            std::io::Cursor::new(second),
            "workflow/run-mounts:second",
            None,
            &std::collections::BTreeMap::new(),
            None,
            true,
        )
        .await?;

    let locked = |tag: &'static str| async move {
        let context = archive_context(
            b"FROM workflow/alpine:test\nRUN --mount=type=cache,id=workflow-locked,target=/cache,sharing=locked n=$(cat /cache/count 2>/dev/null || echo 0); sleep 1; echo $((n + 1)) > /cache/count\n",
            &[],
        )?;
        client
            .images()
            .build_target(
                std::io::Cursor::new(context),
                tag,
                None,
                &std::collections::BTreeMap::new(),
                None,
                true,
            )
            .await?;
        Ok::<_, Error>(())
    };
    tokio::try_join!(locked("workflow/locked:a"), locked("workflow/locked:b"))?;
    let verify = archive_context(
        b"FROM workflow/alpine:test\nRUN --mount=type=cache,id=workflow-locked,target=/cache,sharing=locked test \"$(cat /cache/count)\" = 2\n",
        &[],
    )?;
    client
        .images()
        .build_target(
            std::io::Cursor::new(verify),
            "workflow/locked:verified",
            None,
            &std::collections::BTreeMap::new(),
            None,
            true,
        )
        .await?;
    Ok(())
}

pub(super) async fn automatic_platform(client: &Client) -> Result<(), Error> {
    let context = archive_context(
        b"FROM --platform=$BUILDPLATFORM workflow/alpine:test AS tools\nRUN echo TOOL > /tool\nFROM --platform=$TARGETPLATFORM workflow/alpine:test\nARG BUILDPLATFORM\nARG TARGETPLATFORM\nARG TARGETOS\nARG TARGETARCH\nARG TARGETVARIANT\nCOPY --from=tools /tool /tool\nENV BUILD=$BUILDPLATFORM TARGET=$TARGETPLATFORM OS=$TARGETOS ARCH=$TARGETARCH VARIANT=$TARGETVARIANT\nCMD sh -c \"cat /tool; echo $TARGET\"\n",
        &[],
    )?;
    client
        .images()
        .build(std::io::Cursor::new(context), "workflow/platform:test", None)
        .await?;
    let image = client.images().inspect("workflow/platform:test").await?;
    let environment = image
        .config
        .env
        .iter()
        .filter_map(|value| value.split_once('='))
        .collect::<std::collections::BTreeMap<_, _>>();
    let target = environment.get("TARGET").copied().unwrap_or_default();
    let build = environment.get("BUILD").copied().unwrap_or_default();
    if target != build
        || !target.starts_with("linux/")
        || environment.get("OS") != Some(&"linux")
        || environment.get("ARCH").is_none_or(|arch| arch.is_empty())
    {
        return Err(format!("automatic platform arguments mismatch: {environment:?}").into());
    }
    Ok(())
}
