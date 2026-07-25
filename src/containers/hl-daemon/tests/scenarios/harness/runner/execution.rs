use std::{fs, path::Path, time::Duration};

use crate::{
    contract::{Check, Resource, Scenario, Service, Step},
    fixture::Fixture,
};
use hl_container::{
    Console, ContainerSpec, ExecSpec, ExecState, ExitStatus, Isolation, Process, Resources,
    Sandbox, Size, Stream,
};

use super::{
    evidence::{exit_code, service_logs, signal, CommandOutcome},
    resources::acquire,
    Error, Runner, MATERIALIZE_TIMEOUT,
};

impl Runner<'_> {
    pub(super) async fn execute(&self, case: &Scenario) -> Result<(), Error> {
        let _resources = acquire(case).await?;
        if let Step::Host(command) = &case.step {
            return Err(format!("host contract is not translated to Rust: {command}").into());
        }
        if let Step::Api(operation) = &case.step {
            return Err(format!("API contract is not translated to Rust: {operation}").into());
        }
        let platform = self.target.platform();
        let fixture = tokio::time::timeout(
            MATERIALIZE_TIMEOUT,
            Fixture::materialize_for(case.image, &platform),
        )
        .await
        .map_err(|_| {
            format!(
                "image materialization timed out after {} seconds",
                MATERIALIZE_TIMEOUT.as_secs()
            )
        })??;
        let name = case.id.replace('/', "-");
        let primary = self.execute_fixture(case, &name, &fixture).await;
        let cleanup = self
            .containers
            .remove_force(&name)
            .await
            .map(|_| ())
            .map_err(Into::into);
        let release = fixture.release();
        combine(primary, cleanup, release)
    }

    async fn execute_fixture(
        &self,
        case: &Scenario,
        name: &str,
        fixture: &Fixture,
    ) -> Result<(), Error> {
        let initial = match &case.step {
            Step::Run(_) => process(case, fixture)?,
            Step::Exec(_) => runtime_process(
                Process::new("/bin/sh").args(["-c", "while :; do sleep 3600; done"]),
                case,
                fixture,
            )?,
            Step::Host(_) | Step::Api(_) => unreachable!(),
        };
        let resources = if case.resources.contains(&Resource::ProcessHeavy) {
            Resources {
                cpu_count: 2,
                ..Resources::default()
            }
        } else {
            Resources::default()
        };
        self.containers
            .create(
                ContainerSpec::from_directory(fixture.path(), initial)
                    .name(name)
                    .guest(self.target.guest())
                    .resources(resources)
                    .isolation(Isolation {
                        sandbox: Sandbox::Disabled,
                        network_isolated: case.resources.contains(&Resource::HostPort),
                        ..Isolation::default()
                    }),
            )
            .await?;
        self.containers.start(name).await?;
        if let Some(service) = &case.service {
            self.start_service(case, name, fixture, service).await?;
        }
        match &case.step {
            Step::Exec(script) => {
                let result = self.execute_command(case, name, fixture, script).await;
                if result.is_err() && case.service.is_some() {
                    return result.map_err(|error| {
                        format!(
                            "{error}; service diagnostics: {}",
                            service_logs(fixture, case.service.as_ref().unwrap())
                        )
                        .into()
                    });
                }
                result
            }
            Step::Run(_) => self.wait_initial(case, name).await,
            Step::Host(_) | Step::Api(_) => unreachable!(),
        }
    }

    async fn start_service(
        &self,
        case: &Scenario,
        name: &str,
        fixture: &Fixture,
        service: &Service,
    ) -> Result<(), Error> {
        if service.attempts == 0 {
            return Err("service readiness attempts must be greater than zero".into());
        }
        let startup = match self
            .execute_script(name, fixture, &service.startup, case.operation_timeout())
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(self
                    .service_operation_error(name, fixture, service, 0, error)
                    .await);
            }
        };
        if startup.status != ExitStatus::Code(0) {
            return Err(self
                .service_error(name, fixture, service, 0, &startup)
                .await);
        }
        let mut last = None;
        for attempt in 1..=service.attempts {
            let probe = match self
                .execute_script(name, fixture, &service.probe, case.operation_timeout())
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err(self
                        .service_operation_error(name, fixture, service, attempt, error)
                        .await);
                }
            };
            if probe.status == ExitStatus::Code(0) {
                return Ok(());
            }
            last = Some((attempt, probe));
            if attempt < service.attempts {
                tokio::time::sleep(Duration::from_millis(service.delay_ms)).await;
            }
        }
        let (attempt, probe) = last.expect("positive attempt count records a probe");
        Err(self
            .service_error(name, fixture, service, attempt, &probe)
            .await)
    }

    async fn service_error(
        &self,
        name: &str,
        fixture: &Fixture,
        service: &Service,
        attempts: u32,
        probe: &CommandOutcome,
    ) -> Error {
        let server = self.containers.inspect(name).await.map_or_else(
            |error| format!("inspect-error:{error}"),
            |value| format!("{:?}", value.state),
        );
        format!(
            "service readiness failed attempts={attempts}/{} last_status={:?} last_exit={} last_signal={} last_stdout={:?} last_stderr={:?} server_status={server}; service_logs: {}",
            service.attempts,
            probe.status,
            exit_code(probe.status),
            signal(probe.status),
            String::from_utf8_lossy(&probe.stdout),
            String::from_utf8_lossy(&probe.stderr),
            service_logs(fixture, service),
        )
        .into()
    }

    async fn service_operation_error(
        &self,
        name: &str,
        fixture: &Fixture,
        service: &Service,
        attempts: u32,
        error: Error,
    ) -> Error {
        let server = self.containers.inspect(name).await.map_or_else(
            |inspect| format!("inspect-error:{inspect}"),
            |value| format!("{:?}", value.state),
        );
        format!(
            "service operation failed attempts={attempts}/{} error={error} server_status={server}; service_logs: {}",
            service.attempts,
            service_logs(fixture, service),
        )
        .into()
    }

    async fn wait_initial(&self, case: &Scenario, name: &str) -> Result<(), Error> {
        let status = tokio::time::timeout(
            Duration::from_secs(case.timeout_seconds),
            self.containers.wait(name),
        )
        .await
        .map_err(|_| format!("timed out after {} seconds", case.timeout_seconds))??;
        let logs = self.containers.logs(name).await?;
        verify(
            case,
            status,
            &format!(
                "{}{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            ),
        )
    }

    async fn execute_command(
        &self,
        case: &Scenario,
        name: &str,
        fixture: &Fixture,
        script: &str,
    ) -> Result<(), Error> {
        let outcome = self
            .execute_script(name, fixture, script, case.operation_timeout())
            .await?;
        if outcome.timed_out {
            return Err(format!(
                "timed out after {} seconds; last_status={:?} stdout={:?} stderr={:?}",
                case.timeout_seconds,
                outcome.status,
                String::from_utf8_lossy(&outcome.stdout),
                String::from_utf8_lossy(&outcome.stderr)
            )
            .into());
        }
        verify(
            case,
            outcome.status,
            &format!(
                "{}{}",
                String::from_utf8_lossy(&outcome.stdout),
                String::from_utf8_lossy(&outcome.stderr)
            ),
        )
    }

    async fn execute_script(
        &self,
        name: &str,
        fixture: &Fixture,
        script: &str,
        timeout: Duration,
    ) -> Result<CommandOutcome, Error> {
        let case = Scenario::new("service/internal", "service/internal");
        let process =
            runtime_process(Process::new("/bin/sh").args(["-c", script]), &case, fixture)?;
        let execution = self
            .containers
            .executions()
            .create(name, ExecSpec::new(process))
            .await?;
        let mut session = self.containers.executions().start(&execution.id).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut entries = Vec::new();
        let mut timed_out = false;
        loop {
            match tokio::time::timeout_at(deadline, session.next()).await {
                Ok(Ok(Some(entry))) => entries.push(entry),
                Ok(Ok(None)) => break,
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => {
                    timed_out = true;
                    break;
                }
            }
        }
        let finished = self.containers.executions().inspect(&execution.id).await?;
        let status = match finished.state {
            ExecState::Exited { result, .. } => result,
            _ if timed_out => ExitStatus::Fault {
                status: -1,
                detail: 0,
            },
            state => return Err(format!("exec ended in state {state:?}").into()),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for entry in entries {
            match entry.stream {
                Stream::Stdout => stdout.extend(entry.bytes),
                Stream::Stderr => stderr.extend(entry.bytes),
            }
        }
        Ok(CommandOutcome {
            status,
            stdout,
            stderr,
            timed_out,
        })
    }
}

fn process(case: &Scenario, fixture: &Fixture) -> Result<Process, Error> {
    let Step::Run(arguments) = &case.step else {
        unreachable!()
    };
    let mut argv = fixture.runtime().entrypoint.clone();
    if arguments.is_empty() {
        argv.extend(fixture.runtime().command.iter().cloned());
    } else {
        argv.extend(arguments.iter().cloned());
    }
    let (program, arguments) = argv
        .split_first()
        .ok_or("image and scenario have empty argv")?;
    runtime_process(
        Process::new(program).args(arguments.iter().cloned()),
        case,
        fixture,
    )
}

fn runtime_process(
    mut process: Process,
    case: &Scenario,
    fixture: &Fixture,
) -> Result<Process, Error> {
    for (key, value) in &fixture.runtime().environment {
        process = process.env(key, value);
    }
    for (key, value) in &case.environment {
        process = process.env(key, value);
    }
    if !fixture.runtime().working_directory.is_empty() {
        process = process.working_dir(&fixture.runtime().working_directory);
    }
    if !fixture.runtime().user.is_empty() {
        let (uid, gid) = user(&fixture.runtime().user, fixture.path())?;
        process = process.user(uid, gid);
    }
    if case.terminal {
        if !fixture.runtime().environment.contains_key("TERM")
            && !case.environment.contains_key("TERM")
        {
            process = process.env("TERM", "xterm");
        }
        process = process.console(Console {
            stdin: false,
            terminal: Some(Size::default()),
        });
    }
    Ok(process)
}

fn user(value: &str, root: &Path) -> Result<(i32, i32), Error> {
    let (account, requested_group) = value.split_once(':').unwrap_or((value, ""));
    let (uid, default_gid) = match account.parse::<i32>() {
        Ok(uid) => (uid, uid),
        Err(_) => passwd(account, root)?,
    };
    let gid = if requested_group.is_empty() {
        default_gid
    } else if let Ok(gid) = requested_group.parse::<i32>() {
        gid
    } else {
        group(requested_group, root)?
    };
    Ok((uid, gid))
}

fn passwd(name: &str, root: &Path) -> Result<(i32, i32), Error> {
    for line in fs::read_to_string(root.join("etc/passwd"))?.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.first() == Some(&name) && fields.len() >= 4 {
            return Ok((fields[2].parse()?, fields[3].parse()?));
        }
    }
    Err(format!("image account {name:?} is not resolvable").into())
}

fn group(name: &str, root: &Path) -> Result<i32, Error> {
    for line in fs::read_to_string(root.join("etc/group"))?.lines() {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.first() == Some(&name) && fields.len() >= 3 {
            return Ok(fields[2].parse()?);
        }
    }
    Err(format!("image group {name:?} is not resolvable").into())
}

fn combine(
    primary: Result<(), Error>,
    cleanup: Result<(), Error>,
    release: Result<(), Error>,
) -> Result<(), Error> {
    let mut errors = Vec::new();
    if let Err(error) = primary {
        errors.push(format!("execution: {error}"));
    }
    if let Err(error) = cleanup {
        errors.push(format!("container cleanup: {error}"));
    }
    if let Err(error) = release {
        errors.push(format!("fixture release: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; ").into())
    }
}

fn verify(case: &Scenario, status: ExitStatus, output: &str) -> Result<(), Error> {
    let expected_exit = case
        .checks
        .iter()
        .find_map(|check| match check {
            Check::Exit(code) => Some(*code),
            _ => None,
        })
        .unwrap_or(0);
    let checks = case.checks.iter().all(|check| match check {
        Check::Contains(marker) => output.contains(marker),
        Check::Equals(expected) => output == expected,
        Check::Exit(_) => true,
    });
    if status == ExitStatus::Code(expected_exit) && checks {
        Ok(())
    } else {
        Err(format!(
            "status={status:?} expected_exit={expected_exit} checks={:?} output={output:?}",
            case.checks
        )
        .into())
    }
}
