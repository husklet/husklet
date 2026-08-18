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
const READINESS_SQL: &str = "SELECT CASE WHEN pg_is_in_recovery() THEN 0 ELSE 1 END";
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
    let outcome = append_failure_diagnostics(outcome, || async {
            let diagnostics = tokio::time::timeout(Duration::from_secs(3), fixture.failure_diagnostics())
                .await
                .unwrap_or_else(|_| "PostgreSQL failure diagnostics exceeded 3s".to_owned());
            diagnostics
        })
        .await;
    finish(outcome, fixture.cleanup().await)
}

async fn append_failure_diagnostics<F, D>(outcome: Result<(), Error>, diagnostics: F) -> Result<(), Error>
where
    F: FnOnce() -> D,
    D: Future<Output = String>,
{
    match outcome {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("{error}; {}", diagnostics().await).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn final_server_readiness_rejects_temporary_server_row() {
        assert!(!process::readiness_succeeded("0\n"));
        assert!(process::readiness_succeeded("1\n"));
        let command = process::readiness_command();
        assert!(command.starts_with(
            "test \"$(sed -n '1p' '/var/lib/postgresql/data/postmaster.pid')\" = 1 && exec "
        ));
        assert!(command.contains(READINESS_SQL));
    }

    #[test]
    fn postmaster_guard_prevents_client_execution_until_pid_one() {
        let directory = tempfile::tempdir().unwrap();
        let postmaster = directory.path().join("postmaster.pid");
        std::fs::write(&postmaster, "1508937\n").unwrap();
        let command = process::readiness_command_with(&postmaster.to_string_lossy(), "printf reached");
        let rejected = std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(rejected.stdout.is_empty());

        std::fs::write(&postmaster, "1\n").unwrap();
        let accepted = std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert!(accepted.status.success());
        assert_eq!(accepted.stdout, b"reached");
    }

    #[tokio::test]
    async fn failure_diagnostics_are_lazy_and_preserve_both_snapshots() {
        let calls = AtomicUsize::new(0);
        append_failure_diagnostics(Ok(()), || async {
            calls.fetch_add(1, Ordering::Relaxed);
            "unreachable".to_owned()
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let report = process::failure_diagnostic_report(
            "immediate[container=Running]",
            "container_wait=still-running-after-1s",
            "settled[container=Exited]",
        );
        assert_eq!(
            report,
            "immediate[container=Running]; container_wait=still-running-after-1s; settled[container=Exited]"
        );
        let error = append_failure_diagnostics(Err("primary".into()), || async {
            calls.fetch_add(1, Ordering::Relaxed);
            report
        })
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(error.starts_with("primary; immediate["));
        assert!(error.contains("container_wait=still-running-after-1s"));
        assert!(error.contains("settled[container=Exited]"));
    }
}

struct Fixture {
    _work: tempfile::TempDir,
    rootfs: std::path::PathBuf,
    state: std::path::PathBuf,
    containers: Containers,
    guest: Guest,
    postgres_version: String,
    diagnostic_execs: std::sync::Mutex<std::collections::BTreeMap<&'static str, ExecId>>,
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
