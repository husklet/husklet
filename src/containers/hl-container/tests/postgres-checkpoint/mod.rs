//! PostgreSQL checkpoint acceptance fixture and lifecycle assertions.

mod fixture;
mod lifecycle;
mod process;
mod support;

use hl_container::{
    Config, ContainerSpec, Containers, ExecId, ExecSpec, Guest, Isolation, Process, Sandbox, Signal, Stream, Streams,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    io::Read as _,
    path::Path,
    time::{Duration, Instant},
};

use support::*;

type Error = Box<dyn std::error::Error>;
const CONTAINER: &str = "postgres-checkpoint-acceptance";
const PHASE: Duration = Duration::from_secs(90);
const PROBE: Duration = Duration::from_secs(30);
const CLEANUP: Duration = Duration::from_secs(30);
const ADVISORY_LOCK: i64 = 7_331_904_221;
const POSTGRES_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const POSTGRES_AMD64_DIGEST: &str = "sha256:075f7ba66bc9b3ce7d6b8b635208ff61cd7cf1a67d71ec530eec5d7ae0cbe571";
const POSTGRES_ARM64_DIGEST: &str = "sha256:738d1359df5aa0b6d50a9071e989c49fdd39152a2a805c6ff131bf5e2243e0b3";

#[tokio::test]
#[ignore = "requires HL_POSTGRES_ROOTFS_ARCHIVE containing a pinned postgres:16-alpine rootfs"]
async fn postgres_survives_three_product_checkpoint_cycles() -> Result<(), Error> {
    let fixture = Fixture::new().await?;
    let outcome = bounded(
        "complete PostgreSQL acceptance",
        Duration::from_secs(420),
        fixture.run(),
    )
    .await;
    finish(outcome, fixture.cleanup().await)
}

struct Fixture {
    _work: tempfile::TempDir,
    rootfs: std::path::PathBuf,
    state: std::path::PathBuf,
    containers: Containers,
    guest: Guest,
    postgres_version: String,
}

struct CycleContext<'a> {
    run_id: &'a str,
    system_identifier: &'a str,
    identity_start: &'a str,
    postmaster_pid: &'a str,
    persistent: ExecId,
    sleeper: ExecId,
    sleeper_name: &'a str,
    session_pid: &'a str,
    session: Option<hl_container::Session>,
    previous_tokens: std::collections::BTreeMap<&'static str, (u64, String)>,
}

struct CycleWitness {
    waiter: ExecId,
    roles_before: String,
    sleeper_identity: String,
    waiter_identity: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureManifest {
    image: String,
    image_digest: String,
    archive_sha256: String,
    postgres_major: u16,
    postgres_version: String,
    architecture: String,
}
