use futures_util::{StreamExt as _, stream::FuturesUnordered};

use super::super::{ApiError, ApiResult};
use crate::api::Container;

const SIZE_PARALLELISM_MAX: usize = 8;

pub(in super::super) async fn summaries(
    containers: &hl_container::Containers,
    values: Vec<hl_container::Container>,
    include_size: bool,
) -> ApiResult<Vec<Container>> {
    if !include_size {
        let mut summaries = Vec::with_capacity(values.len());
        for value in values {
            let mounts = super::mount::points(containers, &value.spec.mounts).await?;
            let mut summary = Container::from(value);
            summary.metadata.mounts = mounts;
            summaries.push(summary);
        }
        return Ok(summaries);
    }
    let parallelism = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(SIZE_PARALLELISM_MAX);
    let capacity = values.len();
    let mut values = values.into_iter().enumerate();
    let mut pending = FuturesUnordered::new();
    for (index, value) in values.by_ref().take(parallelism) {
        pending.push(scan_summary(containers, index, value));
    }
    let mut summaries = Vec::with_capacity(capacity);
    let mut error = None;
    while let Some((index, value, result)) = pending.next().await {
        match result {
            Ok((usage, mounts)) => {
                let mut summary = Container::from(value);
                summary.size(usage);
                summary.metadata.mounts = mounts;
                summaries.push((index, summary));
            }
            Err(failure) if error.is_none() => error = Some(failure),
            Err(_) => {}
        }
        if error.is_none()
            && let Some((index, value)) = values.next()
        {
            pending.push(scan_summary(containers, index, value));
        }
    }
    if let Some(error) = error {
        return Err(error);
    }
    summaries.sort_unstable_by_key(|(index, _)| *index);
    Ok(summaries.into_iter().map(|(_, summary)| summary).collect())
}

async fn scan_summary(
    containers: &hl_container::Containers,
    index: usize,
    value: hl_container::Container,
) -> (
    usize,
    hl_container::Container,
    ApiResult<(hl_container::FilesystemUsage, Vec<crate::api::MountPoint>)>,
) {
    let result = async {
        let usage = containers
            .filesystem_usage(value.id.as_str())
            .await
            .map_err(ApiError::container)?;
        let mounts = super::mount::points(containers, &value.spec.mounts).await?;
        Ok((usage, mounts))
    }
    .await;
    (index, value, result)
}
