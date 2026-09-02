//! Reading container state on behalf of an extension.

use std::{sync::Arc, time::Duration};

use hl_client::model::{Container, InspectContainer, List};
use hl_extension::port::{
    ContainerInventory, ContainerOutput, ContainerSummary, ExecutionList, ExecutionSummary, HostError, ProcessList,
};

use super::{Bridge, failure};

/// The container reading port over the workspace's container daemon.
pub struct ContainerCatalog {
    bridge: Arc<Bridge>,
}

impl ContainerCatalog {
    pub(super) fn new(bridge: Arc<Bridge>) -> Self {
        Self { bridge }
    }
}

impl ContainerInventory for ContainerCatalog {
    /// Lists every container, stopped ones included: an extension that can only
    /// see running containers cannot tell "gone" from "not started".
    ///
    /// # Errors
    /// Returns a host failure from the container daemon.
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
        let client = self.bridge.client();
        let containers = self
            .bridge
            .wait(client.containers().list(List::default().all()))
            .map_err(|error| failure(&error))?;
        Ok(containers.iter().map(summary).collect())
    }

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
        let client = self.bridge.client();
        let container = self
            .bridge
            .wait(client.containers().inspect(id))
            .map_err(|error| failure(&error))?;
        Ok(inspection(&container))
    }

    fn processes(&self, id: &str) -> Result<ProcessList, HostError> {
        let client = self.bridge.client();
        let table = self
            .bridge
            .wait(client.containers().top(id))
            .map_err(|error| failure(&error))?;
        Ok(ProcessList {
            titles: table.titles,
            processes: table.processes,
        })
    }

    fn logs(&self, id: &str, stdout: bool, stderr: bool) -> Result<ContainerOutput, HostError> {
        let client = self.bridge.client();
        // Inspect first: a stopped process has stable output. If it stops after
        // this observation we conservatively answer `eof: false` rather than
        // claiming a replay that raced its final bytes was complete.
        let inspection = self
            .bridge
            .wait(client.containers().inspect(id))
            .map_err(|error| failure(&error))?;
        let logs = self
            .bridge
            .wait(client.containers().logs(id, stdout, stderr))
            .map_err(|error| failure(&error))?;
        Ok(output(
            logs.stdout,
            logs.stderr,
            inspection.state.activity.running || inspection.state.activity.restarting,
        ))
    }

    fn execution(&self, id: &str) -> Result<ExecutionSummary, HostError> {
        let client = self.bridge.client();
        let execution = self
            .bridge
            .wait(client.executions().inspect(id))
            .map_err(|error| failure(&error))?;
        Ok(execution_summary(execution))
    }

    fn executions(&self) -> Result<ExecutionList, HostError> {
        const LIMIT: usize = 1024;
        let client = self.bridge.client();
        let catalogue = self.bridge.wait(client.executions().list(LIMIT as u16)).map_err(|error| failure(&error))?;
        Ok(ExecutionList { executions: catalogue.executions.into_iter().map(execution_summary).collect(), truncated: catalogue.truncated })
    }

    fn execution_logs(&self, id: &str, stdout: bool, stderr: bool) -> Result<ContainerOutput, HostError> {
        let client = self.bridge.client();
        let execution = self.bridge.wait(client.executions().inspect(id)).map_err(|error| failure(&error))?;
        let logs = self.bridge.wait(client.executions().logs(id)).map_err(|error| failure(&error))?;
        Ok(output(
            if stdout { logs.stdout } else { Vec::new() },
            if stderr { logs.stderr } else { Vec::new() },
            execution.running,
        ))
    }

    fn execution_wait(&self, id: &str, timeout_ms: u32) -> Result<ExecutionSummary, HostError> {
        let client = self.bridge.client();
        self.bridge.wait(async {
            tokio::time::timeout(Duration::from_millis(u64::from(timeout_ms)), client.executions().wait(id)).await
        }).map_err(|_| HostError::Conflict(format!("execution {id} did not stop within {timeout_ms}ms")))?
            .map_err(|error| failure(&error))?;
        let execution = self.bridge.wait(client.executions().inspect(id)).map_err(|error| failure(&error))?;
        Ok(execution_summary(execution))
    }
}

fn execution_summary(execution: hl_client::model::ExecInspect) -> ExecutionSummary {
        let command = std::iter::once(execution.process.entrypoint.clone())
            .chain(execution.process.arguments.clone())
            .filter(|part| !part.is_empty())
            .collect();
        ExecutionSummary {
            id: execution.id,
            container_id: execution.container_id,
            running: execution.running,
            exit_code: execution.exit_code,
            pid: execution.pid,
            command,
            user: execution.process.user,
        }
}

/// Per-stream wire bound. The client already bounds the HTTP response; this
/// smaller limit bounds what one extension reply retains and serializes.
const OUTPUT_BYTES: usize = 512 * 1024;

fn bounded(bytes: Vec<u8>) -> (Vec<u8>, bool) {
    if bytes.len() <= OUTPUT_BYTES {
        return (bytes, false);
    }
    (bytes[bytes.len() - OUTPUT_BYTES..].to_vec(), true)
}

fn output(stdout: Vec<u8>, stderr: Vec<u8>, running: bool) -> ContainerOutput {
    let (stdout, stdout_truncated) = bounded(stdout);
    let (stderr, stderr_truncated) = bounded(stderr);
    ContainerOutput {
        stdout,
        stderr,
        truncated: stdout_truncated || stderr_truncated,
        stdout_truncated,
        stderr_truncated,
        eof: !running,
    }
}

/// Maps a Docker list entry onto the protocol's container view.
fn summary(container: &Container) -> ContainerSummary {
    ContainerSummary {
        id: container.details.metadata.id.clone(),
        name: container.names.first().map_or_else(
            || container.details.metadata.id.clone(),
            |name| name.trim_start_matches('/').to_owned(),
        ),
        image: container.details.metadata.image.clone(),
        state: container.state.clone(),
        created: container.created,
    }
}

/// Maps a Docker inspection onto the same view.
///
/// Inspection reports creation as an RFC 3339 instant while the list reports
/// epoch seconds, so the timestamp is converted here rather than leaking two
/// spellings of one field to extensions.
fn inspection(container: &InspectContainer) -> ContainerSummary {
    ContainerSummary {
        id: container.details.metadata.id.clone(),
        name: container.name.trim_start_matches('/').to_owned(),
        image: container.details.metadata.image.clone(),
        state: container.state.status.clone(),
        created: epoch_seconds(&container.created).unwrap_or_default(),
    }
}

/// Converts `YYYY-MM-DDThh:mm:ss[.fraction][Z]` to whole seconds since the epoch.
///
/// Returns `None` for anything it cannot read, because a creation instant is
/// descriptive and a container that exists must still be reportable.
fn epoch_seconds(timestamp: &str) -> Option<i64> {
    let (date, clock) = timestamp.split_once('T')?;
    let clock = clock.trim_end_matches('Z');
    let clock = clock.split_once('.').map_or(clock, |(whole, _)| whole);
    let mut fields = date.splitn(3, '-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: i64 = fields.next()?.parse().ok()?;
    let day: i64 = fields.next()?.parse().ok()?;
    let mut parts = clock.splitn(3, ':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    Some(civil_days(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days from the epoch to a proleptic Gregorian date, by the standard
/// era-based formulation, so no calendar table or clock crate is needed.
fn civil_days(year: i64, month: i64, day: i64) -> i64 {
    let shifted = year - i64::from(month <= 2);
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::{OUTPUT_BYTES, bounded, civil_days, epoch_seconds, inspection, output, summary};
    use hl_client::model::{Container, InspectContainer};

    fn listing() -> Container {
        serde_json::from_value(serde_json::json!({
            "Id": "c0ffee",
            "Names": ["/demo"],
            "Image": "ubuntu:24.04",
            "Command": "sleep",
            "Created": 1_700_000_000_i64,
            "State": "running",
            "Status": "Up 3 minutes",
            "Ports": [],
            "Mounts": [],
            "Labels": {}
        }))
        .expect("container listing")
    }

    #[test]
    fn a_listing_maps_onto_the_protocol_view() {
        let mapped = summary(&listing());
        assert_eq!(mapped.id, "c0ffee");
        assert_eq!(mapped.name, "demo", "the Docker leading slash is not an identity");
        assert_eq!(mapped.image, "ubuntu:24.04");
        assert_eq!(mapped.state, "running");
        assert_eq!(mapped.created, 1_700_000_000);
    }

    #[test]
    fn a_listing_without_a_name_falls_back_to_its_identity() {
        let mut container = listing();
        container.names.clear();
        assert_eq!(summary(&container).name, "c0ffee");
    }

    #[test]
    fn an_inspection_maps_its_state_and_instant() {
        let container: InspectContainer = serde_json::from_value(serde_json::json!({
            "Id": "c0ffee",
            "Image": "ubuntu:24.04",
            "Mounts": [],
            "Path": "/bin/sh",
            "Args": [],
            "Name": "/demo",
            "Created": "2023-11-14T22:13:20.000000000Z",
            "State": {
                "Status": "exited",
                "Running": false,
                "Paused": false,
                "Restarting": false,
                "OOMKilled": false,
                "Dead": false,
                "Pid": 0,
                "ExitCode": 0,
                "Error": "",
                "StartedAt": "",
                "FinishedAt": ""
            },
            "RestartCount": 0,
            "Config": { "ExposedPorts": {}, "Labels": {}, "StopSignal": "SIGTERM", "StopTimeout": 10 },
            "HostConfig": {
                "NetworkMode": "bridge",
                "AutoRemove": false,
                "RestartPolicy": { "Name": "no", "MaximumRetryCount": 0 }
            },
            "NetworkSettings": { "Ports": {}, "Networks": {} }
        }))
        .expect("container inspection");

        let mapped = inspection(&container);
        assert_eq!(mapped.name, "demo");
        assert_eq!(mapped.state, "exited");
        assert_eq!(mapped.created, 1_700_000_000, "the same instant the listing reports");
    }

    #[test]
    fn known_instants_convert() {
        assert_eq!(civil_days(1970, 1, 1), 0);
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2000-03-01T00:00:01Z"), Some(951_868_801));
        assert_eq!(epoch_seconds("1969-12-31T23:59:59Z"), Some(-1));
    }

    #[test]
    fn an_unreadable_instant_does_not_hide_the_container() {
        assert_eq!(epoch_seconds("not a timestamp"), None);
        assert_eq!(epoch_seconds("2023-11-14"), None);
    }

    #[test]
    fn extension_log_answers_keep_the_newest_bounded_bytes() {
        let bytes: Vec<u8> = (0..OUTPUT_BYTES + 7).map(|index| (index % 251) as u8).collect();
        let expected = bytes[7..].to_vec();
        let (answer, truncated) = bounded(bytes);
        assert!(truncated);
        assert_eq!(answer, expected);
    }

    #[test]
    fn output_names_each_cut_and_never_calls_a_running_empty_replay_eof() {
        let running = output(Vec::new(), vec![b'e'; OUTPUT_BYTES + 1], true);
        assert!(!running.stdout_truncated);
        assert!(running.stderr_truncated);
        assert!(running.truncated);
        assert!(!running.eof, "empty stdout from a running process is not EOF");

        let complete = output(Vec::new(), Vec::new(), false);
        assert!(complete.eof);
        assert!(!complete.truncated);
    }
}
