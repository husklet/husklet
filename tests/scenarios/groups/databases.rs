//! Database compatibility contracts.
//!
//! These cases deliberately retain their original OCI fixture identity. Running a database case
//! against the generic Alpine rootfs would test a missing binary, not the database contract. The
//! command/output matrix is declarative; only the timeout-cleanup probe stays in Rust.

use crate::contract;
use hl_container::{ContainerSpec, Containers, Isolation, Process, Sandbox};
use std::{path::Path, time::Duration};

type Error = Box<dyn std::error::Error>;

pub(crate) fn group() -> contract::Group {
    contract::Group::new(
        "databases",
        crate::manifest::load(include_str!("../fixtures/databases-core.yaml"))
            .expect("the checked-in database manifest must satisfy the schema"),
    )
}

pub(crate) async fn cleanup_probe(containers: &Containers, alpine: &Path) -> Result<(), Error> {
    let name = "databases-timeout-cleanup";
    containers
        .create(
            ContainerSpec::from_directory(alpine, Process::new("/bin/sh").args(["-c", "sleep 30"]))
                .name(name)
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    ..Isolation::default()
                }),
        )
        .await?;
    containers.start(name).await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), containers.wait(name))
            .await
            .is_err()
    );
    remove_created(containers, name).await?;
    assert!(containers.inspect(name).await.is_err());
    println!("PASS database-cleanup");
    Ok(())
}

async fn remove_created(containers: &Containers, name: &str) -> Result<(), String> {
    containers
        .remove_force(name)
        .await
        .map(|_| ())
        .map_err(|error| format!("remove {name}: {error}"))
}
