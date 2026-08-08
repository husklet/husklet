use super::{Client, Error, append};

pub(super) async fn cache(client: &Client) -> Result<(), Error> {
    let context = cache_context("default")?;
    let first = client
        .images()
        .build(std::io::Cursor::new(context.clone()), "workflow/cache-one:test", None)
        .await?;
    let second = client
        .images()
        .build(std::io::Cursor::new(context.clone()), "workflow/cache-two:test", None)
        .await?;
    if first != second {
        return Err("identical build did not reuse its content cache".into());
    }
    let uncached = client
        .images()
        .build_target(
            std::io::Cursor::new(context.clone()),
            "workflow/cache-uncached:test",
            None,
            &std::collections::BTreeMap::new(),
            None,
            true,
        )
        .await?;
    if uncached == first {
        return Err("no-cache build reused cached random layer content".into());
    }

    let left = client.clone();
    let right = client.clone();
    let concurrent = cache_context("concurrent")?;
    let a = concurrent.clone();
    let (left, right) = tokio::join!(
        async move {
            left.images()
                .build(std::io::Cursor::new(a), "workflow/cache-left:test", None)
                .await
        },
        async move {
            right
                .images()
                .build(std::io::Cursor::new(concurrent), "workflow/cache-right:test", None)
                .await
        }
    );
    if left? != right? {
        return Err("concurrent identical builds published different cache results".into());
    }

    let arguments = std::collections::BTreeMap::from([("VALUE".into(), "changed".into())]);
    let changed = client
        .images()
        .build_with(
            std::io::Cursor::new(context),
            "workflow/cache-argument:test",
            None,
            &arguments,
        )
        .await?;
    if changed == first {
        return Err("build-arg change did not invalidate cache".into());
    }
    let visible = client.images().list().await?;
    if visible
        .iter()
        .flat_map(|image| &image.repo_tags)
        .any(|tag| tag.contains("hl-build-cache"))
    {
        return Err("internal build-cache tag leaked through image listing".into());
    }
    let _ = client.images().prune_builds().await?;
    Ok(())
}

fn cache_context(default: &str) -> Result<Vec<u8>, Error> {
    let dockerfile = format!(
        "FROM workflow/alpine:test\nARG VALUE={default}\nRUN printf %s \"$VALUE\" > /value; head -c 16 /dev/urandom > /nonce\nCMD cat /value\n"
    );
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        append(&mut archive, "Dockerfile", dockerfile.as_bytes())?;
        archive.finish()?;
    }
    Ok(bytes)
}
