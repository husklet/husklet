use super::{definition::ScenarioCase, process};
use crate::suite::Error;
use hl_container::{Containers, ExecSpec, ExitStatus, Size, Stream, Streams};
use hl_images::RuntimeConfig;
use serde::Deserialize;
use std::{
    path::Path,
    time::{Duration, Instant},
};

const MAX_ARGUMENTS: usize = 256;
const MAX_FIELD: usize = 4096;
const MAX_STEPS: usize = 64;
const MAX_TEXT: usize = 64 * 1024;
const MAX_WAIT_MS: u64 = 60_000;
const CAPTURE_LIMIT: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    argv: Vec<String>,
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default = "default_columns")]
    columns: u16,
    steps: Vec<StepDefinition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepDefinition {
    write: Option<Text>,
    resize: Option<Resize>,
    close: Option<Empty>,
    await_output: Option<Await>,
    reject_output: Option<Text>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Text {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resize {
    rows: u16,
    columns: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Await {
    contains: String,
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Debug)]
pub struct Action {
    pub argv: Vec<String>,
    pub rows: u16,
    pub columns: u16,
    pub steps: Vec<Step>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Step {
    Write(Vec<u8>),
    Resize { rows: u16, columns: u16 },
    Close,
    AwaitOutput { contains: Vec<u8>, timeout_ms: u64 },
    RejectOutput(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metric {
    pub operation: &'static str,
    pub elapsed_us: u64,
    pub bytes_written: u64,
    pub bytes_read: u64,
    pub succeeded: bool,
}

trait Clock {
    fn now_us(&self) -> u64;
}

struct MonotonicClock(Instant);

impl Clock for MonotonicClock {
    fn now_us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

pub(super) async fn run(
    containers: &Containers,
    case: &ScenarioCase,
    runtime: &RuntimeConfig,
    rootfs: &Path,
    name: &str,
    action: &Action,
    metrics: &mut Vec<Metric>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Error> {
    let process = process::terminal(case, action, runtime, rootfs)?;
    let executions = containers.executions();
    let execution = executions
        .create(
            name,
            ExecSpec::new(process).streams(Streams {
                stdin: true,
                stdout: true,
                stderr: true,
            }),
        )
        .await?;
    let initial_size = Size::new(action.rows, action.columns)?;
    let mut session = executions.start_at(&execution.id, initial_size).await?;
    let input = session.input();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut rejected = Vec::new();
    let clock = MonotonicClock(Instant::now());

    for step in &action.steps {
        let started = clock.now_us();
        let read_before = transcript_bytes(&stdout, &stderr);
        match step {
            Step::Write(bytes) => {
                let result = input.write(bytes.clone()).await;
                record(metrics, &clock, started, "write", bytes.len(), 0, result.is_ok());
                result?;
            }
            Step::Resize { rows, columns } => {
                let result = executions.resize(&execution.id, Size::new(*rows, *columns)?).await;
                record(metrics, &clock, started, "resize", 0, 0, result.is_ok());
                result?;
            }
            Step::Close => {
                input.close().await;
                record(metrics, &clock, started, "close", 0, 0, true);
            }
            Step::AwaitOutput { contains, timeout_ms } => {
                let result = await_output(
                    &mut session,
                    &mut stdout,
                    &mut stderr,
                    contains,
                    Duration::from_millis(*timeout_ms),
                )
                .await;
                record(
                    metrics,
                    &clock,
                    started,
                    "await_output",
                    0,
                    transcript_bytes(&stdout, &stderr).saturating_sub(read_before),
                    result.is_ok(),
                );
                result?;
            }
            Step::RejectOutput(bytes) => {
                rejected.push((bytes.as_slice(), metrics.len()));
                record(metrics, &clock, started, "reject_output", 0, 0, true);
            }
        }
    }
    let started = clock.now_us();
    let read_before = transcript_bytes(&stdout, &stderr);
    while let Some(entry) = session.next().await? {
        capture(entry.stream, entry.bytes, &mut stdout, &mut stderr)?;
    }
    record(
        metrics,
        &clock,
        started,
        "drain",
        0,
        transcript_bytes(&stdout, &stderr).saturating_sub(read_before),
        true,
    );
    for (bytes, metric_index) in rejected {
        if transcript_contains(&stdout, &stderr, bytes) {
            metrics[metric_index].succeeded = false;
            return Err(format!(
                "{} terminal transcript unexpectedly contains {:?}",
                case.id,
                String::from_utf8_lossy(bytes)
            )
            .into());
        }
    }
    let status = executions.wait(&execution.id).await?;
    Ok((status, stdout, stderr))
}

fn record(
    metrics: &mut Vec<Metric>,
    clock: &impl Clock,
    started: u64,
    operation: &'static str,
    bytes_written: usize,
    bytes_read: usize,
    succeeded: bool,
) {
    metrics.push(Metric {
        operation,
        elapsed_us: clock.now_us().saturating_sub(started),
        bytes_written: u64::try_from(bytes_written).unwrap_or(u64::MAX),
        bytes_read: u64::try_from(bytes_read).unwrap_or(u64::MAX),
        succeeded,
    });
}

fn transcript_bytes(stdout: &[u8], stderr: &[u8]) -> usize {
    stdout.len().saturating_add(stderr.len())
}

async fn await_output(
    session: &mut hl_container::Session,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    expected: &[u8],
    timeout: Duration,
) -> Result<(), Error> {
    let receive = async {
        while !transcript_contains(stdout, stderr, expected) {
            let Some(entry) = session.next().await? else {
                return Err::<(), Error>(
                    format!("terminal ended before output {:?}", String::from_utf8_lossy(expected)).into(),
                );
            };
            capture(entry.stream, entry.bytes, stdout, stderr)?;
        }
        Ok(())
    };
    tokio::time::timeout(timeout, receive)
        .await
        .map_err(|_| format!("timed out awaiting terminal output after {} ms", timeout.as_millis()))??;
    Ok(())
}

fn capture(stream: Stream, bytes: Vec<u8>, stdout: &mut Vec<u8>, stderr: &mut Vec<u8>) -> Result<(), Error> {
    let total = stdout
        .len()
        .checked_add(stderr.len())
        .and_then(|size| size.checked_add(bytes.len()))
        .ok_or("terminal transcript size overflow")?;
    if total > CAPTURE_LIMIT {
        return Err(format!("terminal transcript exceeded {CAPTURE_LIMIT} bytes").into());
    }
    match stream {
        Stream::Stdout => stdout.extend(bytes),
        Stream::Stderr => stderr.extend(bytes),
    }
    Ok(())
}

fn transcript_contains(stdout: &[u8], stderr: &[u8], expected: &[u8]) -> bool {
    expected.is_empty()
        || stdout.windows(expected.len()).any(|window| window == expected)
        || stderr.windows(expected.len()).any(|window| window == expected)
}

impl Definition {
    pub(super) fn validate(self, id: &str) -> Result<Action, Error> {
        validate_argv(id, &self.argv)?;
        validate_size(id, self.rows, self.columns)?;
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(format!("{id} terminal action has an invalid step count").into());
        }
        let steps = self
            .steps
            .into_iter()
            .map(|step| step.validate(id))
            .collect::<Result<Vec<_>, Error>>()?;
        if steps.iter().filter(|step| matches!(step, Step::Close)).count() > 1 {
            return Err(format!("{id} terminal action closes input more than once").into());
        }
        if let Some(close) = steps.iter().position(|step| matches!(step, Step::Close))
            && steps[close + 1..].iter().any(|step| matches!(step, Step::Write(_)))
        {
            return Err(format!("{id} terminal action writes after closing input").into());
        }
        Ok(Action {
            argv: self.argv,
            rows: self.rows,
            columns: self.columns,
            steps,
        })
    }
}

impl StepDefinition {
    fn validate(self, id: &str) -> Result<Step, Error> {
        let count = usize::from(self.write.is_some())
            + usize::from(self.resize.is_some())
            + usize::from(self.close.is_some())
            + usize::from(self.await_output.is_some())
            + usize::from(self.reject_output.is_some());
        if count != 1 {
            return Err(format!("{id} terminal step must select exactly one operation").into());
        }
        match (
            self.write,
            self.resize,
            self.close,
            self.await_output,
            self.reject_output,
        ) {
            (Some(Text { text }), None, None, None, None) if bounded(&text, true) => Ok(Step::Write(text.into_bytes())),
            (None, Some(size), None, None, None) => {
                validate_size(id, size.rows, size.columns)?;
                Ok(Step::Resize {
                    rows: size.rows,
                    columns: size.columns,
                })
            }
            (None, None, Some(Empty {}), None, None) => Ok(Step::Close),
            (None, None, None, Some(Await { contains, timeout_ms }), None)
                if bounded(&contains, false) && (1..=MAX_WAIT_MS).contains(&timeout_ms) =>
            {
                Ok(Step::AwaitOutput {
                    contains: contains.into_bytes(),
                    timeout_ms,
                })
            }
            (None, None, None, None, Some(Text { text })) if bounded(&text, false) => {
                Ok(Step::RejectOutput(text.into_bytes()))
            }
            _ => Err(format!("{id} has an empty or invalid terminal step").into()),
        }
    }
}

fn validate_argv(id: &str, argv: &[String]) -> Result<(), Error> {
    if argv.first().is_none_or(String::is_empty)
        || argv.len() > MAX_ARGUMENTS
        || argv.iter().any(|value| value.len() > MAX_FIELD || value.contains('\0'))
        || argv.iter().map(String::len).sum::<usize>() > MAX_TEXT
    {
        Err(format!("{id} terminal action has invalid argv").into())
    } else {
        Ok(())
    }
}

fn validate_size(id: &str, rows: u16, columns: u16) -> Result<(), Error> {
    if rows == 0 || columns == 0 {
        Err(format!("{id} terminal size must be nonzero").into())
    } else {
        Ok(())
    }
}

fn bounded(value: &str, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty()) && value.len() <= MAX_TEXT
}

const fn default_rows() -> u16 {
    24
}

const fn default_columns() -> u16 {
    80
}

#[cfg(test)]
mod tests {
    use super::{Clock, Metric, record};
    use std::{cell::Cell, sync::OnceLock, time::Instant};

    fn lifecycle(boundary: &'static str) {
        static STARTED: OnceLock<Instant> = OnceLock::new();
        let elapsed_us = STARTED
            .get_or_init(Instant::now)
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX));
        let lane = std::env::var("HL_PTY_LANE").unwrap_or_else(|_| "single".to_owned());
        eprintln!(
            "HL_PTY_EVENT lane={lane} boundary={boundary} elapsed_us={elapsed_us} pid={}",
            std::process::id()
        );
    }

    struct FakeClock(Cell<u64>);

    impl Clock for FakeClock {
        fn now_us(&self) -> u64 {
            self.0.get()
        }
    }

    #[test]
    fn step_metrics_use_injected_monotonic_time_and_byte_counts() {
        let clock = FakeClock(Cell::new(17));
        let mut metrics = Vec::new();
        record(&mut metrics, &clock, 10, "write", 3, 5, true);
        assert_eq!(
            metrics,
            [Metric {
                operation: "write",
                elapsed_us: 7,
                bytes_written: 3,
                bytes_read: 5,
                succeeded: true,
            }]
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn alpine_terminal_flows_through_public_container_session_apis() {
        use crate::{runtime::image::TestImage, suite::Target};
        use hl_container::{Config, Console, ContainerSpec, ExecSpec, ExitStatus, Process, Size, Streams};

        lifecycle("image.begin");
        let image = TestImage::materialize("alpine", &Target::Arm64.platform())
            .await
            .unwrap();
        lifecycle("image.end");
        let state = tempfile::tempdir().unwrap();
        lifecycle("provider.begin");
        let containers = hl_container::Containers::builder(Config::new(state.path()))
            .build()
            .await
            .unwrap();
        lifecycle("provider.end");
        let initial = Process::new("/bin/sh").args(["-c", "while :; do sleep 3600; done"]);
        lifecycle("container_create.begin");
        containers
            .create(
                ContainerSpec::from_directory(image.path(), initial)
                    .name("terminal-public-api")
                    .guest(Target::Arm64.guest()),
            )
            .await
            .unwrap();
        lifecycle("container_create.end");
        lifecycle("container_start.begin");
        containers.start("terminal-public-api").await.unwrap();
        lifecycle("container_start.end");
        let process = Process::new("/bin/sh")
            .args(["-c", "printf READY; IFS= read -r line; printf 'GOT=%s' \"$line\""])
            .console(Console {
                stdin: true,
                terminal: Some(Size::new(24, 80).unwrap()),
            });
        lifecycle("execution_create.begin");
        let execution = containers
            .executions()
            .create(
                "terminal-public-api",
                ExecSpec::new(process).streams(Streams {
                    stdin: true,
                    stdout: true,
                    stderr: true,
                }),
            )
            .await
            .unwrap();
        lifecycle("execution_create.end");
        lifecycle("execution_start.begin");
        let mut session = containers
            .executions()
            .start_at(&execution.id, Size::new(24, 80).unwrap())
            .await
            .unwrap();
        lifecycle("execution_start.end");
        lifecycle("input_write.begin");
        session.write(b"hello\r".to_vec()).await.unwrap();
        lifecycle("input_write.end");
        lifecycle("input_close.begin");
        session.close().await;
        lifecycle("input_close.end");
        let mut transcript = Vec::new();
        lifecycle("transcript_drain.begin");
        while let Some(entry) = session.next().await.unwrap() {
            transcript.extend(entry.bytes);
        }
        lifecycle("transcript_drain.end");
        lifecycle("execution_wait.begin");
        assert_eq!(
            containers.executions().wait(&execution.id).await.unwrap(),
            ExitStatus::Code(0)
        );
        lifecycle("execution_wait.end");
        assert!(transcript.windows(5).any(|bytes| bytes == b"READY"));
        assert!(transcript.windows(9).any(|bytes| bytes == b"GOT=hello"));
        lifecycle("remove_force.begin");
        containers.remove_force("terminal-public-api").await.unwrap();
        lifecycle("remove_force.end");
        assert!(matches!(
            containers.inspect("terminal-public-api").await,
            Err(hl_container::Error::NotFound(_))
        ));
        assert!(containers.executions().list().await.unwrap().is_empty());
        lifecycle("image_release.begin");
        image.release().unwrap();
        lifecycle("image_release.end");
    }
}
