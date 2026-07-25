use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use crate::contract::{Resource, Scenario};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::Error;

pub(super) async fn acquire(case: &Scenario) -> Result<Vec<OwnedSemaphorePermit>, Error> {
    static HOST_PORT: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let mut permits = Vec::new();
    if case.resources.contains(&Resource::HostPort) {
        permits.push(
            HOST_PORT
                .get_or_init(|| Arc::new(Semaphore::new(1)))
                .clone()
                .acquire_owned()
                .await?,
        );
    }
    Ok(permits)
}

pub(crate) async fn test_resources() -> Result<(), Error> {
    super::test_resume_outcomes();
    let port = Scenario::new("collision", "fixture").resource(Resource::HostPort);
    let plain = Scenario::new("plain", "fixture");
    let first = acquire(&port).await?;
    if tokio::time::timeout(Duration::from_millis(20), acquire(&port))
        .await
        .is_ok()
    {
        return Err("conflicting host-port cases ran concurrently".into());
    }
    tokio::time::timeout(Duration::from_millis(20), acquire(&plain)).await??;
    drop(first);
    tokio::time::timeout(Duration::from_millis(100), acquire(&port)).await??;
    Ok(())
}
