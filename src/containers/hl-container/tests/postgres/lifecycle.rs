use super::*;

impl Fixture {
    pub(super) async fn run(&self) -> Result<(), Error> {
        let run_id = format!("{}-{}", std::process::id(), unix_millis());
        let process = Process::new("/bin/sh")
            .args([
                "-ceu",
                "test -d /var/lib/postgresql && test -w /var/lib/postgresql; marker=/var/lib/postgresql/.acceptance-started; mkdir -p /var/lib/postgresql/data; if [ -e \"$marker\" ]; then echo FRESH_START_FORBIDDEN >&2; exit 97; fi; printf '%s\\n' \"$HL_ACCEPTANCE_RUN\" > \"$marker.tmp\"; mv \"$marker.tmp\" \"$marker\"; exec /usr/local/bin/docker-entrypoint.sh postgres",
            ])
            .env("POSTGRES_HOST_AUTH_METHOD", "trust")
            .env("PGDATA", "/var/lib/postgresql/data")
            .env("POSTGRES_INITDB_ARGS", "--auth=trust")
            .env("POSTGRES_PASSWORD", "acceptance-only");
        let process = process
            .env("HL_ACCEPTANCE_RUN", &run_id)
            .env("PATH", POSTGRES_PATH)
            .env("LANG", "en_US.utf8")
            .env("PG_MAJOR", "16")
            .env("PG_VERSION", &self.postgres_version);
        let spec = ContainerSpec::from_directory(&self.rootfs, process)
            .name(CONTAINER)
            .guest(self.guest)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                read_only_root: false,
                network_isolated: true,
                seccomp_baseline: hl_container::SeccompBaseline::Container,
            });
        self.containers.create(spec).await?;
        bounded("initial PostgreSQL start", PHASE, self.containers.start(CONTAINER)).await?;
        self.wait_ready().await?;
        let server_version = self.query("SHOW server_version").await?;
        require(
            server_version.trim() == self.postgres_version,
            format!(
                "running PostgreSQL version {:?} differs from pinned {:?}",
                server_version.trim(),
                self.postgres_version
            ),
        )?;

        let identity = self.query("SELECT system_identifier||':'||extract(epoch from pg_postmaster_start_time())::bigint||':'||pg_backend_pid() FROM pg_control_system()").await?;
        let identity_fields = identity.trim().split(':').collect::<Vec<_>>();
        require(
            identity_fields.len() == 3,
            format!("incomplete PostgreSQL identity: {identity:?}"),
        )?;
        let postmaster_pid = self.exec("sed -n '1p' /var/lib/postgresql/data/postmaster.pid").await?;
        self.query(&format!(
            "CREATE TABLE acceptance_ledger(run text, cycle int, phase text, payload text, PRIMARY KEY(run,cycle,phase)); INSERT INTO acceptance_ledger VALUES ('{run_id}',0,'init','init'); CHECKPOINT;"
        )).await?;

        let (exec, mut session) = self.persistent_client().await?;
        let sleeper_name = format!("husklet-sleeper-{run_id}");
        let sleeper = self
            .containers
            .executions()
            .create(
                CONTAINER,
                ExecSpec::new(
                    Process::new("/usr/local/bin/psql")
                        .env("PGAPPNAME", &sleeper_name)
                        .args([
                            "-X",
                            "-qAt",
                            "-v",
                            "ON_ERROR_STOP=1",
                            "-U",
                            "postgres",
                            "-c",
                            "SELECT pg_sleep(100000)",
                        ]),
                ),
            )
            .await?;
        self.remember_execution("sleeper", &sleeper.id);
        let sleep_attachment = self.containers.executions().start(&sleeper.id).await?;
        drop(sleep_attachment);
        session.write(format!(
            "CREATE TEMP TABLE session_nonce(v text); INSERT INTO session_nonce VALUES ('{run_id}'); SELECT pg_advisory_lock({ADVISORY_LOCK}); SELECT 'SESSION_READY:'||pg_backend_pid();\n"
        )).await?;
        let session_ready = read_until(&mut session, "SESSION_READY:", PROBE).await?;
        let session_pid = session_ready
            .lines()
            .find_map(|line| line.strip_prefix("SESSION_READY:"))
            .ok_or("persistent backend PID was not reported")?
            .trim()
            .to_owned();

        let mut context = CycleContext {
            run_id: &run_id,
            system_identifier: identity_fields[0],
            identity_start: identity_fields[1],
            postmaster_pid: postmaster_pid.trim(),
            persistent: exec,
            sleeper: sleeper.id,
            sleeper_name: &sleeper_name,
            session_pid: &session_pid,
            session: Some(session),
            previous_tokens: std::collections::BTreeMap::new(),
        };
        for cycle in 1..=3 {
            Box::pin(self.checkpoint_cycle(cycle, &mut context)).await?;
        }
        let mut session = context
            .session
            .take()
            .ok_or("persistent session missing after checkpoint cycles")?;
        let exec = context.persistent;

        session
            .write("SELECT pg_advisory_unlock_all(); SELECT 'CLIENT_DONE';\n")
            .await?;
        read_until(&mut session, "CLIENT_DONE", PROBE).await?;
        session.close().await;
        let _ = bounded(
            "persistent client exit",
            PROBE,
            self.containers.executions().wait(&exec),
        )
        .await?;
        self.exec("rm -f /var/lib/postgresql/.acceptance-started; test ! -e /var/lib/postgresql/.acceptance-started")
            .await?;
        self.exec("/usr/local/bin/su-exec postgres /usr/local/bin/pg_ctl -D /var/lib/postgresql/data -m fast -w stop")
            .await?;
        bounded("wait for clean PostgreSQL stop", PROBE, self.containers.wait(CONTAINER)).await?;
        bounded("cold PostgreSQL restart", PHASE, self.containers.start(CONTAINER)).await?;
        self.wait_ready().await?;
        let final_rows = self
            .query(&format!("SELECT count(*) FROM acceptance_ledger WHERE run='{run_id}'"))
            .await?;
        require(
            final_rows.trim() == "10",
            format!("cold restart durability mismatch: {final_rows:?}"),
        )?;
        require(
            self.exec("cat /var/lib/postgresql/.acceptance-started").await?.trim() == run_id,
            "cold restart did not recreate the expected PID1 marker",
        )?;
        Ok(())
    }

    async fn checkpoint_cycle(&self, cycle: u32, context: &mut CycleContext<'_>) -> Result<(), Error> {
        let mut session = context
            .session
            .take()
            .ok_or("persistent session missing before checkpoint cycle")?;
        let witness = self.prepare_checkpoint_cycle(cycle, context, &mut session).await?;
        let close_started = Instant::now();
        let before_generation = self.containers.inspect(CONTAINER).await?.generation;
        Box::pin(self.capture_checkpoint_cycle(
            cycle,
            context,
            &witness.waiter,
            before_generation,
            close_started,
            session,
        ))
        .await?;
        let reopen_started = Instant::now();
        let (mut session, mut waiter_session) = self
            .restore_checkpoint_cycle(cycle, context, &witness, before_generation)
            .await?;
        require(
            reopen_started.elapsed() <= PHASE,
            format!("cycle {cycle}: reopen exceeded {PHASE:?}"),
        )?;
        eprintln!(
            "postgres-checkpoint cycle={cycle} reopen_ms={} postmaster_pid={} backend_pid={}",
            reopen_started.elapsed().as_millis(),
            context.postmaster_pid.trim(),
            context.session_pid
        );
        self.complete_checkpoint_cycle(cycle, context, &witness.waiter, &mut session, &mut waiter_session)
            .await?;
        context.session = Some(session);
        Ok(())
    }

    async fn prepare_checkpoint_cycle(
        &self,
        cycle: u32,
        context: &CycleContext<'_>,
        session: &mut hl_container::Session,
    ) -> Result<CycleWitness, Error> {
        session.write(format!(
            "INSERT INTO acceptance_ledger VALUES ('{}',{cycle},'pre','pre-{cycle}'); BEGIN; INSERT INTO acceptance_ledger VALUES ('{}',{cycle},'inflight','inflight-{cycle}'); SELECT 'PREPARED:{cycle}';\n",
            context.run_id, context.run_id
        )).await?;
        read_until(session, &format!("PREPARED:{cycle}"), PROBE).await?;
        let lock = self
            .query(&format!("SELECT pg_try_advisory_lock({ADVISORY_LOCK})"))
            .await?;
        require(
            lock.trim() == "f",
            format!("cycle {cycle}: advisory lock was not owned by persistent client"),
        )?;
        let roles_before = self.background_roles().await?;
        require(
            roles_before.lines().count() >= 4,
            format!("cycle {cycle}: PostgreSQL background process tree absent: {roles_before:?}"),
        )?;
        let waiter_name = format!("husklet-waiter-{}-{cycle}", context.run_id);
        let waiter = self.lock_waiter(cycle, &waiter_name).await?;
        let sleeper_identity = self.client_identity(context.sleeper_name).await?;
        let waiter_identity = self.client_identity(&waiter_name).await?;
        Ok(CycleWitness {
            waiter,
            roles_before,
            sleeper_identity,
            waiter_identity,
        })
    }

    async fn capture_checkpoint_cycle(
        &self,
        cycle: u32,
        context: &mut CycleContext<'_>,
        waiter: &ExecId,
        before_generation: u64,
        close_started: Instant,
        session: hl_container::Session,
    ) -> Result<(), Error> {
        bounded(
            "checkpoint all product processes",
            PHASE,
            self.containers.checkpoint_all(Duration::from_secs(30)),
        )
        .await?;
        let captured = self.containers.inspect(CONTAINER).await?;
        let container_token = captured
            .checkpoint
            .as_ref()
            .ok_or("container checkpoint token missing")?;
        let client_token = self
            .containers
            .executions()
            .inspect(&context.persistent)
            .await?
            .checkpoint
            .ok_or("persistent client checkpoint token missing")?;
        let sleeper_token = self
            .containers
            .executions()
            .inspect(&context.sleeper)
            .await?
            .checkpoint
            .ok_or("sleeper checkpoint token missing")?;
        let waiter_token = self
            .containers
            .executions()
            .inspect(waiter)
            .await?
            .checkpoint
            .ok_or("lock waiter checkpoint token missing")?;
        // One image per process domain. An exec session joins the container's freeze and opens no
        // image of its own, so every member's token names the container's namespace: that single
        // image is where its state was actually captured. Four distinct namespaces would mean four
        // images and four independently-committed generations, which is the shape that produced
        // captures no restore could validate. Assert the shared namespace positively.
        let current_namespaces = [
            &container_token.namespace,
            &client_token.namespace,
            &sleeper_token.namespace,
            &waiter_token.namespace,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        require(
            current_namespaces.len() == 1 && current_namespaces.contains(&container_token.namespace),
            format!("cycle {cycle}: domain member tokens did not name the container image: {current_namespaces:?}"),
        )?;
        require(
            captured.generation == before_generation,
            format!("cycle {cycle}: checkpoint unexpectedly changed launch generation"),
        )?;
        for (role, token) in [
            ("container", container_token),
            ("persistent", &client_token),
            ("sleeper", &sleeper_token),
            ("waiter", &waiter_token),
        ] {
            let current = (token.created_at_ms, token.namespace.clone());
            if let Some(previous) = context.previous_tokens.insert(role, current.clone()) {
                require(
                    token.created_at_ms > previous.0,
                    format!("cycle {cycle}: {role} checkpoint timestamp did not strictly advance"),
                )?;
                require(
                    previous != current,
                    format!("cycle {cycle}: {role} checkpoint token did not advance uniquely"),
                )?;
            }
        }
        require(
            close_started.elapsed() <= PHASE,
            format!("cycle {cycle}: checkpoint exceeded {PHASE:?}"),
        )?;
        drop(session);
        let checkpoint_hash = checkpoint_artifact_hash(&self.state, [&container_token.namespace])?;
        eprintln!(
            "postgres-checkpoint cycle={cycle} close_ms={} state_sha256={checkpoint_hash}",
            close_started.elapsed().as_millis()
        );
        Ok(())
    }

    async fn restore_checkpoint_cycle(
        &self,
        cycle: u32,
        context: &CycleContext<'_>,
        witness: &CycleWitness,
        before_generation: u64,
    ) -> Result<(hl_container::Session, hl_container::Session), Error> {
        bounded("restore PostgreSQL primary", PHASE, self.containers.start(CONTAINER)).await?;
        // start() returns once the restore launch is dispatched, not once the tree has resumed. Without
        // this wait every later probe races the restore driver and reports "connection refused" while the
        // driver is still working -- which hid the driver's own refusal for the whole of this blocker.
        self.wait_ready().await?;
        let failures = bounded(
            "restore PostgreSQL clients",
            PHASE,
            self.containers.executions().restore_checkpoints(),
        )
        .await?;
        require(
            failures.is_empty(),
            format!("cycle {cycle}: exec restore failures: {failures:?}"),
        )?;
        let restored = self.containers.inspect(CONTAINER).await?;
        require(
            restored.checkpoint.is_none() && restored.generation == before_generation + 1,
            format!("cycle {cycle}: container token was not consumed exactly once"),
        )?;
        require(
            self.containers
                .executions()
                .inspect(&context.persistent)
                .await?
                .checkpoint
                .is_none()
                && self
                    .containers
                    .executions()
                    .inspect(&context.sleeper)
                    .await?
                    .checkpoint
                    .is_none()
                && self
                    .containers
                    .executions()
                    .inspect(&witness.waiter)
                    .await?
                    .checkpoint
                    .is_none(),
            format!("cycle {cycle}: exec checkpoint token remained after restore"),
        )?;
        require(
            self.containers
                .executions()
                .inspect(&context.persistent)
                .await?
                .state
                .is_active(),
            format!("cycle {cycle}: persistent PostgreSQL client was not restored"),
        )?;
        let session = bounded(
            "reattach persistent PostgreSQL client",
            PROBE,
            self.containers.executions().attach(&context.persistent, None),
        )
        .await?;
        let waiter_session = bounded(
            "reattach advisory-lock waiter",
            PROBE,
            self.containers.executions().attach(&witness.waiter, None),
        )
        .await?;
        require(
            self.containers
                .executions()
                .inspect(&context.sleeper)
                .await?
                .state
                .is_active(),
            format!("cycle {cycle}: active PostgreSQL client was not restored"),
        )?;
        require(
            self.containers
                .executions()
                .inspect(&witness.waiter)
                .await?
                .state
                .is_active(),
            format!("cycle {cycle}: advisory-lock waiter was not restored"),
        )?;
        require(
            self.background_roles().await? == witness.roles_before,
            format!("cycle {cycle}: PostgreSQL background PID/role identity changed"),
        )?;
        require(
            self.client_identity(context.sleeper_name).await? == witness.sleeper_identity,
            format!("cycle {cycle}: sleeper PostgreSQL PID/role changed"),
        )?;
        require(
            self.client_identity(&format!("husklet-waiter-{}-{cycle}", context.run_id))
                .await?
                == witness.waiter_identity,
            format!("cycle {cycle}: waiter PostgreSQL PID/role changed"),
        )?;
        Ok((session, waiter_session))
    }

    async fn complete_checkpoint_cycle(
        &self,
        cycle: u32,
        context: &CycleContext<'_>,
        waiter: &ExecId,
        session: &mut hl_container::Session,
        waiter_session: &mut hl_container::Session,
    ) -> Result<(), Error> {
        session.write(format!(
            "SELECT 'CONTINUITY:'||pg_backend_pid()||':'||(SELECT v FROM session_nonce); COMMIT; INSERT INTO acceptance_ledger VALUES ('{}',{cycle},'post','post-{cycle}'); SELECT 'RESTORED:{cycle}';\n",
            context.run_id
        )).await?;
        let continuity = read_until(session, &format!("RESTORED:{cycle}"), PROBE).await?;
        require(
            continuity.contains(&format!("CONTINUITY:{}:{}", context.session_pid, context.run_id)),
            format!("cycle {cycle}: fresh client/server fallback detected: {continuity:?}"),
        )?;
        require(
            self.query("SELECT extract(epoch from pg_postmaster_start_time())::bigint::text")
                .await?
                .trim()
                == context.identity_start,
            format!("cycle {cycle}: postmaster restarted instead of restoring"),
        )?;
        require(
            self.query("SELECT system_identifier::text FROM pg_control_system()")
                .await?
                .trim()
                == context.system_identifier,
            format!("cycle {cycle}: PostgreSQL system identifier changed"),
        )?;
        require(
            self.exec("sed -n '1p' /var/lib/postgresql/data/postmaster.pid")
                .await?
                .trim()
                == context.postmaster_pid.trim(),
            format!("cycle {cycle}: postmaster PID lineage changed"),
        )?;
        require(
            self.exec("cat /var/lib/postgresql/.acceptance-started").await?.trim() == context.run_id,
            format!("cycle {cycle}: PID1 fresh-start marker changed"),
        )?;
        session
            .write(format!("SELECT pg_advisory_unlock_all(); SELECT 'UNLOCKED:{cycle}';\n"))
            .await?;
        read_until(session, &format!("UNLOCKED:{cycle}"), PROBE).await?;
        let waiter_output = read_until(waiter_session, &format!("WAITER:{cycle}"), PROBE).await?;
        require(
            waiter_output.contains(&format!("WAITER:{cycle}")),
            format!("cycle {cycle}: shared advisory-lock waiter did not resume"),
        )?;
        waiter_session.close().await;
        let waiter_status = bounded(
            "wait for advisory-lock waiter",
            PROBE,
            self.containers.executions().wait(waiter),
        )
        .await?;
        require(
            waiter_status == hl_container::ExitStatus::Code(0),
            format!("cycle {cycle}: advisory-lock waiter exited as {waiter_status:?}"),
        )?;
        self.containers.executions().remove(waiter).await?;
        session
            .write(format!(
                "SELECT pg_advisory_lock({ADVISORY_LOCK}); SELECT 'RELOCKED:{cycle}';\n"
            ))
            .await?;
        read_until(session, &format!("RELOCKED:{cycle}"), PROBE).await?;
        let exact = self
            .query(&format!(
                "SELECT cycle||':'||phase||':'||payload FROM acceptance_ledger WHERE run='{}' ORDER BY cycle,phase",
                context.run_id
            ))
            .await?;
        let mut expected = vec!["0:init:init".to_owned()];
        for completed in 1..=cycle {
            expected.extend([
                format!("{completed}:inflight:inflight-{completed}"),
                format!("{completed}:post:post-{completed}"),
                format!("{completed}:pre:pre-{completed}"),
            ]);
        }
        require(
            exact.lines().eq(expected.iter().map(String::as_str)),
            format!("cycle {cycle}: exact ledger mismatch: {exact:?}"),
        )?;
        Ok(())
    }
}
