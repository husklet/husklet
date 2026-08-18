use super::*;

impl Fixture {
    pub(super) async fn background_roles(&self) -> Result<String, Error> {
        self.query("SELECT pid||':'||backend_type FROM pg_stat_activity WHERE backend_type <> 'client backend' ORDER BY backend_type,pid").await
    }

    pub(super) async fn lock_waiter(&self, cycle: u32, application_name: &str) -> Result<ExecId, Error> {
        let execution = self
            .containers
            .executions()
            .create(
                CONTAINER,
                ExecSpec::new(
                    Process::new("/usr/local/bin/psql")
                        .env("PGAPPNAME", application_name)
                        .args([
                            "-X",
                            "-qAt",
                            "-v",
                            "ON_ERROR_STOP=1",
                            "-U",
                            "postgres",
                            "-c",
                            &format!("SELECT pg_advisory_lock({ADVISORY_LOCK}); SELECT 'WAITER:{cycle}'"),
                        ]),
                ),
            )
            .await?;
        drop(self.containers.executions().start(&execution.id).await?);
        tokio::time::sleep(Duration::from_millis(100)).await;
        require(
            self.containers
                .executions()
                .inspect(&execution.id)
                .await?
                .state
                .is_active(),
            format!("cycle {cycle}: advisory-lock waiter did not block"),
        )?;
        Ok(execution.id)
    }

    pub(super) async fn client_identity(&self, application_name: &str) -> Result<String, Error> {
        let identity = self
            .query(&format!(
                "SELECT pid||':'||backend_type||':'||application_name FROM pg_stat_activity WHERE application_name={}",
                shell_quote(application_name)
            ))
            .await?;
        require(
            identity.lines().count() == 1,
            format!("expected one PostgreSQL client for {application_name:?}, got {identity:?}"),
        )?;
        Ok(identity)
    }

    pub(super) async fn persistent_client(&self) -> Result<(ExecId, hl_container::Session), Error> {
        let spec = ExecSpec::new(Process::new("/usr/local/bin/psql").args([
            "-X",
            "-qAt",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
        ]))
        .streams(Streams {
            stdin: true,
            stdout: true,
            stderr: true,
        });
        let execution = self.containers.executions().create(CONTAINER, spec).await?;
        let session = self.containers.executions().start(&execution.id).await?;
        Ok((execution.id, session))
    }

    pub(super) async fn query(&self, sql: &str) -> Result<String, Error> {
        self.exec(&format!(
            "/usr/local/bin/psql -X -qAt -v ON_ERROR_STOP=1 -U postgres -c {}",
            shell_quote(sql)
        ))
        .await
    }

    pub(super) async fn exec(&self, command: &str) -> Result<String, Error> {
        let executions = self.containers.executions();
        let execution = executions
            .create(
                CONTAINER,
                ExecSpec::new(Process::new("/bin/sh").env("PATH", POSTGRES_PATH).args(["-c", command])),
            )
            .await?;
        let mut session = executions.start(&execution.id).await?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        while let Some(entry) = bounded("PostgreSQL command output", PROBE, session.next()).await? {
            match entry.stream {
                Stream::Stdout => append_output(&mut stdout, &entry.bytes)?,
                Stream::Stderr => append_output(&mut stderr, &entry.bytes)?,
            }
        }
        let state = executions.inspect(&execution.id).await?;
        require(
            matches!(
                &state.state,
                hl_container::ExecState::Exited {
                    result: hl_container::ExitStatus::Code(0),
                    ..
                }
            ),
            format!("command failed or remained active: {:?}", state.state),
        )?;
        require(
            stderr.is_empty(),
            format!("PostgreSQL command stderr: {}", String::from_utf8_lossy(&stderr)),
        )?;
        Ok(String::from_utf8(stdout)?)
    }

    pub(super) async fn wait_ready(&self) -> Result<(), Error> {
        let started = Instant::now();
        let mut attempt = 0_u64;
        let mut distinct = Vec::<(String, u64)>::new();
        let mut diagnostics = "diagnostics not yet captured".to_owned();
        let mut next_diagnostic = Duration::ZERO;
        loop {
            attempt += 1;
            let remaining = PROBE.saturating_sub(started.elapsed());
            let probe = tokio::time::timeout(
                remaining.min(Duration::from_secs(1)),
                self.query("SELECT CASE WHEN pg_is_in_recovery() THEN 0 ELSE 1 END"),
            )
            .await;
            let last = match probe {
                Ok(Ok(value)) if value.trim() == "1" => return Ok(()),
                Ok(Ok(value)) => format!("unexpected readiness value {value:?}"),
                Ok(Err(error)) => bounded_text(error.to_string().as_bytes()),
                Err(_) => "readiness query exceeded one second".to_owned(),
            };
            let elapsed = started.elapsed();
            let newly_distinct = if let Some((_, count)) = distinct.iter_mut().find(|(error, _)| error == &last) {
                *count += 1;
                false
            } else if distinct.len() < 8 {
                distinct.push((last.clone(), 1));
                true
            } else {
                false
            };
            if newly_distinct || attempt % 50 == 0 {
                eprintln!(
                    "postgres-readiness attempt={attempt} elapsed_ms={} error={last:?}",
                    elapsed.as_millis()
                );
            }
            if elapsed >= next_diagnostic && PROBE.saturating_sub(elapsed) >= Duration::from_secs(1) {
                diagnostics = tokio::time::timeout(Duration::from_secs(1), self.readiness_diagnostics())
                    .await
                    .unwrap_or_else(|_| "readiness diagnostics exceeded one second".to_owned());
                next_diagnostic = elapsed + Duration::from_secs(5);
            }
            if elapsed >= PROBE {
                let representatives = distinct
                    .iter()
                    .map(|(error, count)| format!("{count}x {error}"))
                    .collect::<Vec<_>>()
                    .join(" | ");
                return Err(format!(
                    "PostgreSQL readiness exceeded {PROBE:?} after {attempt} attempts; last={last:?}; distinct=[{representatives}]; {diagnostics}"
                )
                .into());
            }
            tokio::time::sleep(PROBE.saturating_sub(started.elapsed()).min(Duration::from_millis(100))).await;
        }
    }

    async fn readiness_diagnostics(&self) -> String {
        let container = tokio::time::timeout(Duration::from_millis(250), self.containers.inspect(CONTAINER)).await;
        let logs = tokio::time::timeout(Duration::from_millis(250), self.containers.logs(CONTAINER)).await;
        let process = tokio::time::timeout(Duration::from_millis(250), self.exec("id; printf 'tmp='; ls -ld /tmp; printf 'socket-dir='; ls -ld /var/run/postgresql 2>&1 || true; printf 'pgdata='; ls -ld /var/lib/postgresql/data 2>&1 || true; printf 'socket='; ls -l /var/run/postgresql/.s.PGSQL.5432 2>&1 || true; printf 'postmaster-pid='; cat /var/lib/postgresql/data/postmaster.pid 2>&1 || true; printf 'processes='; ps -ef 2>&1 || true")).await;
        let (stdout, stderr) = match logs {
            Ok(Ok(logs)) => (bounded_text(&logs.stdout), bounded_text(&logs.stderr)),
            Ok(Err(error)) => (String::new(), format!("logs unavailable: {error}")),
            Err(_) => (String::new(), "logs timed out after 250ms".to_owned()),
        };
        format!(
            "container={container:?}; pid1_stdout={stdout:?}; pid1_stderr={stderr:?}; permissions_and_processes={process:?}"
        )
    }

    pub(super) async fn cleanup(&self) -> Result<(), Error> {
        let first = tokio::time::timeout(CLEANUP, self.containers.remove_force(CONTAINER)).await;
        if !matches!(first, Ok(Ok(_)) | Ok(Err(hl_container::Error::NotFound(_)))) {
            let signal = tokio::time::timeout(PROBE, self.containers.signal(CONTAINER, Signal::KILL)).await;
            let wait = tokio::time::timeout(PROBE, self.containers.wait(CONTAINER)).await;
            match tokio::time::timeout(CLEANUP, self.containers.remove_force(CONTAINER)).await {
                Ok(Ok(_)) | Ok(Err(hl_container::Error::NotFound(_))) => {}
                Ok(Err(error)) => return Err(format!(
                    "cleanup escalation failed: initial={first:?}, signal={signal:?}, wait={wait:?}, removal={error}"
                )
                .into()),
                Err(_) => {
                    return Err(format!(
                        "cleanup escalation exceeded {CLEANUP:?}: initial={first:?}, signal={signal:?}, wait={wait:?}"
                    )
                    .into());
                }
            }
        }
        let inspect = tokio::time::timeout(PROBE, self.containers.inspect(CONTAINER))
            .await
            .map_err(|_| "final cleanup inspect timed out")?;
        require(
            matches!(inspect, Err(hl_container::Error::NotFound(_))),
            "container record remained after cleanup",
        )?;
        let executions = tokio::time::timeout(PROBE, self.containers.executions().list())
            .await
            .map_err(|_| "final cleanup execution-list timed out")??;
        require(executions.is_empty(), "execution records remained after cleanup")
    }
}
