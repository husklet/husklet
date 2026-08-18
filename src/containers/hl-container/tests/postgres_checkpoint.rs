//! Opt-in PostgreSQL acceptance through the public container checkpoint lifecycle.

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

impl Fixture {
    async fn new() -> Result<Self, Error> {
        let archive = std::env::var_os("HL_POSTGRES_ROOTFS_ARCHIVE")
            .ok_or("HL_POSTGRES_ROOTFS_ARCHIVE must name a pinned postgres:16-alpine rootfs tar.gz")?;
        let manifest_path = std::env::var_os("HL_POSTGRES_FIXTURE_MANIFEST")
            .ok_or("HL_POSTGRES_FIXTURE_MANIFEST must name the pinned fixture JSON")?;
        let manifest: FixtureManifest = serde_json::from_reader(std::fs::File::open(&manifest_path)?)?;
        let guest = guest()?;
        let expected_arch = match guest {
            Guest::X86_64 => "amd64",
            Guest::Aarch64 => "arm64",
        };
        require(
            manifest.postgres_major == 16,
            "fixture manifest must pin PostgreSQL major 16",
        )?;
        require(
            manifest.image == "postgres:16-alpine",
            format!("unexpected fixture image {}", manifest.image),
        )?;
        require(
            manifest.image_digest.starts_with("sha256:") && manifest.image_digest.len() == 71,
            "fixture image_digest must be a sha256 digest",
        )?;
        require(
            manifest.image_digest[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "fixture image_digest must contain 64 lowercase hexadecimal digits",
        )?;
        require(
            manifest.archive_sha256.len() == 64
                && manifest
                    .archive_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "fixture archive_sha256 must contain 64 lowercase hexadecimal digits",
        )?;
        let expected_image_digest = match expected_arch {
            "amd64" => POSTGRES_AMD64_DIGEST,
            "arm64" => POSTGRES_ARM64_DIGEST,
            _ => unreachable!("guest() only returns supported architectures"),
        };
        require(
            manifest.image_digest == expected_image_digest,
            "fixture manifest image digest differs from independent pin",
        )?;
        require(
            manifest.postgres_version.starts_with("16."),
            "fixture must pin an exact PostgreSQL 16 patch version",
        )?;
        require(
            manifest.architecture == expected_arch,
            format!(
                "fixture architecture {} does not match {expected_arch}",
                manifest.architecture
            ),
        )?;
        require(
            file_hash(Path::new(&archive))? == manifest.archive_sha256,
            "PostgreSQL fixture archive digest mismatch",
        )?;
        let work = tempfile::tempdir()?;
        let rootfs = work.path().join("rootfs");
        std::fs::create_dir(&rootfs)?;
        let input = std::fs::File::open(&archive).map_err(|error| {
            format!(
                "open PostgreSQL rootfs archive {}: {error}",
                Path::new(&archive).display()
            )
        })?;
        tar::Archive::new(flate2::read::GzDecoder::new(input)).unpack(&rootfs)?;
        for required in [
            "usr/local/bin/docker-entrypoint.sh",
            "usr/local/bin/postgres",
            "usr/local/bin/psql",
        ] {
            require(
                rootfs.join(required).is_file(),
                format!("fixture is not PostgreSQL: missing /{required}"),
            )?;
        }
        verify_elf_machine(&rootfs.join("usr/local/bin/postgres"), expected_arch)?;
        let state = work.path().join("state");
        let containers = Containers::builder(Config::new(&state)).build().await?;
        Ok(Self {
            _work: work,
            rootfs,
            state,
            containers,
            guest,
            postgres_version: manifest.postgres_version,
        })
    }

    async fn run(&self) -> Result<(), Error> {
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
        let _sleep_attachment = self.containers.executions().start(&sleeper.id).await?;
        drop(_sleep_attachment);
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
            self.checkpoint_cycle(cycle, &mut context).await?;
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
        self.capture_checkpoint_cycle(
            cycle,
            context,
            &witness.waiter,
            before_generation,
            close_started,
            session,
        )
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
        let current_namespaces = [
            &container_token.namespace,
            &client_token.namespace,
            &sleeper_token.namespace,
            &waiter_token.namespace,
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        require(
            current_namespaces.len() == 4,
            format!("cycle {cycle}: checkpoint namespaces were not pairwise unique"),
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
        let checkpoint_hash = checkpoint_artifact_hash(
            &self.state,
            [
                &container_token.namespace,
                &client_token.namespace,
                &sleeper_token.namespace,
                &waiter_token.namespace,
            ],
        )?;
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

    async fn background_roles(&self) -> Result<String, Error> {
        self.query("SELECT pid||':'||backend_type FROM pg_stat_activity WHERE backend_type <> 'client backend' ORDER BY backend_type,pid").await
    }

    async fn lock_waiter(&self, cycle: u32, application_name: &str) -> Result<ExecId, Error> {
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

    async fn client_identity(&self, application_name: &str) -> Result<String, Error> {
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

    async fn persistent_client(&self) -> Result<(ExecId, hl_container::Session), Error> {
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

    async fn query(&self, sql: &str) -> Result<String, Error> {
        self.exec(&format!(
            "/usr/local/bin/psql -X -qAt -v ON_ERROR_STOP=1 -U postgres -c {}",
            shell_quote(sql)
        ))
        .await
    }

    async fn exec(&self, command: &str) -> Result<String, Error> {
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

    async fn wait_ready(&self) -> Result<(), Error> {
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

    async fn cleanup(&self) -> Result<(), Error> {
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

async fn read_until(session: &mut hl_container::Session, marker: &str, timeout: Duration) -> Result<String, Error> {
    bounded("persistent PostgreSQL client output", timeout, async {
        let mut output = Vec::new();
        while let Some(entry) = session.next().await? {
            if entry.stream == Stream::Stdout {
                output.extend(entry.bytes);
            }
            if String::from_utf8_lossy(&output).contains(marker) {
                return Ok::<String, Error>(String::from_utf8(output)?);
            }
        }
        Err(format!("persistent PostgreSQL client ended before {marker:?}").into())
    })
    .await
}

async fn bounded<T, E>(label: &str, timeout: Duration, future: impl Future<Output = Result<T, E>>) -> Result<T, Error>
where
    E: Into<Error>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| format!("{label} exceeded {timeout:?}"))?
        .map_err(Into::into)
}

fn checkpoint_artifact_hash<'a>(
    state: &Path,
    namespaces: impl IntoIterator<Item = &'a String>,
) -> Result<String, Error> {
    fn visit(root: &Path, path: &Path, hash: &mut Sha256) -> Result<(usize, u64), Error> {
        let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut files = 0;
        let mut bytes = 0;
        for entry in entries {
            let path = entry.path();
            hash.update(path.strip_prefix(root)?.as_os_str().as_encoded_bytes());
            if path.is_dir() {
                let nested = visit(root, &path, hash)?;
                files += nested.0;
                bytes += nested.1;
            } else if path.is_file() {
                files += 1;
                let mut file = std::fs::File::open(path)?;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    bytes += count as u64;
                    hash.update(&buffer[..count]);
                }
            }
        }
        Ok((files, bytes))
    }
    let checkpoint_root = state.join("runtime/checkpoints");
    let mut artifacts = namespaces
        .into_iter()
        .map(|namespace| checkpoint_root.join(namespace))
        .collect::<Vec<_>>();
    artifacts.sort();
    let mut hash = Sha256::new();
    for artifact in artifacts {
        require(
            artifact.is_dir(),
            format!("checkpoint artifact is missing: {}", artifact.display()),
        )?;
        hash.update(artifact.strip_prefix(&checkpoint_root)?.as_os_str().as_encoded_bytes());
        let (files, bytes) = visit(&checkpoint_root, &artifact, &mut hash)?;
        require(
            files > 0 && bytes > 0,
            format!(
                "checkpoint artifact contains no nonempty regular files: {}",
                artifact.display()
            ),
        )?;
    }
    Ok(hex_digest(hash.finalize().as_slice()))
}

fn file_hash(path: &Path) -> Result<String, Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex_digest(hash.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn bounded_text(bytes: &[u8]) -> String {
    const LIMIT: usize = 16 * 1024;
    if bytes.len() <= LIMIT {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let half = LIMIT / 2;
    let mut text = String::from_utf8_lossy(&bytes[..half]).into_owned();
    text.push_str(&format!("\n[{} bytes omitted]\n", bytes.len() - LIMIT));
    text.push_str(&String::from_utf8_lossy(&bytes[bytes.len() - half..]));
    text
}

fn append_output(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    const LIMIT: usize = 1024 * 1024;
    require(
        output.len().saturating_add(bytes.len()) <= LIMIT,
        "diagnostic command output exceeded 1 MiB",
    )?;
    output.extend_from_slice(bytes);
    Ok(())
}

fn verify_elf_machine(path: &Path, architecture: &str) -> Result<(), Error> {
    let mut header = [0_u8; 24];
    std::fs::File::open(path)?.read_exact(&mut header)?;
    require(&header[..4] == b"\x7fELF", format!("{} is not ELF", path.display()))?;
    require(header[4] == 2, format!("{} is not ELF64", path.display()))?;
    require(header[5] == 1, format!("{} is not little-endian ELF", path.display()))?;
    require(
        header[6] == 1,
        format!("{} has unsupported ELF identification version", path.display()),
    )?;
    require(
        u32::from_le_bytes([header[20], header[21], header[22], header[23]]) == 1,
        format!("{} has unsupported ELF header version", path.display()),
    )?;
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match architecture {
        "amd64" => 62,
        "arm64" => 183,
        value => return Err(format!("unsupported fixture architecture {value:?}").into()),
    };
    require(
        machine == expected,
        format!("{} has ELF machine {machine}, expected {expected}", path.display()),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn guest() -> Result<Guest, Error> {
    match std::env::var("HL_SCENARIO_TARGET") {
        Ok(value) if value == "amd64" => Ok(Guest::X86_64),
        Ok(value) if value == "arm64" => Ok(Guest::Aarch64),
        Err(std::env::VarError::NotPresent) => Ok(Guest::Aarch64),
        Ok(value) => Err(format!("unsupported HL_SCENARIO_TARGET {value:?}").into()),
        Err(error) => Err(error.into()),
    }
}
fn require(condition: bool, message: impl Into<String>) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into().into()) }
}
fn finish(outcome: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (outcome, cleanup) {
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; cleanup also failed: {cleanup}").into()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
