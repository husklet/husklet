//! Guest process-tree contracts under the production sandbox default.
//!
//! Every workflow in this file runs with `Isolation::default()`, whose `Sandbox::SentryOnly` sends
//! `HL_UNTRUSTED=1` and routes the guest's descriptor authority through the sentry. That is what an
//! ordinary `docker run` gets, so a defect reachable only under it is reachable by every user; the
//! sibling fixtures that set `Sandbox::Disabled` cannot see one.
//!
//! The shapes here are the cheapest guest programs that make a child process name a descriptor its
//! parent created: a pipeline and a command substitution. Both cost the guest a `clone` followed by
//! `dup3` onto an inherited pipe end, which is the exact pair that had no coverage.

use hl_container::{ContainerSpec, Containers, ExitStatus, Process};
use std::time::Duration;
use tempfile::TempDir;

use super::fixture;

type Error = Box<dyn std::error::Error>;

/// Each case is `(name, script, expected stdout)`.
const CASES: [(&str, &str, &str); 3] = [
    ("sandbox-pipeline", "echo alpha | cat", "alpha\n"),
    ("sandbox-substitution", "printf 'F=%s\\n' \"$(echo hi)\"", "F=hi\n"),
    (
        "sandbox-substitution-nonroot",
        "printf 'U=%s F=%s\\n' \"$(id -u)\" \"$(echo hi)\"",
        "U=65534 F=hi\n",
    ),
];

pub(crate) async fn run(containers: &Containers) -> Result<(), Error> {
    let roots = TempDir::new()?;
    for (name, script, expected) in CASES {
        let root = fixture::rootfs(roots.path(), name)?;
        let mut process = Process::new("/bin/sh").args(["-c", script]);
        if name.ends_with("nonroot") {
            process = process.user(65534, 65534);
        }
        containers
            .create(
                ContainerSpec::from_directory(&root, process)
                    .guest(fixture::guest())
                    .name(name),
            )
            .await?;
        containers.start(name).await?;
        // A descriptor the child cannot reach does not fail the child loudly: the parent shell keeps
        // waiting on a pipe nobody will ever close, so the guest hangs rather than exits. Bound the
        // wait so the failure names the container instead of expiring some caller's request budget.
        let status = tokio::time::timeout(Duration::from_secs(30), containers.wait(name))
            .await
            .map_err(|_| format!("{name}: the guest never exited; its child could not inherit a descriptor"))??;
        let logs = containers.logs(name).await?;
        if status != ExitStatus::Code(0) || logs.stdout != expected.as_bytes() {
            return Err(format!(
                "{name}: status={status:?} stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr),
            )
            .into());
        }
        containers.remove_force(name).await?;
        println!("PASS sandbox/{name}");
    }
    Ok(())
}
