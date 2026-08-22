//! Product-path checkpoint acceptance for the workspace domain.

use super::*;
use hl_container::{Config, ContainerSpec, Guest, Isolation, Process, Sandbox};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "HL_PRODUCT_CHECKPOINT_DOMAIN_CHILD";
const FORBID_REMOTE_ENV: &str = "HL_TEST_FORBID_REMOTE_IMAGES";
const CHILD_TEST: &str = "runtime::domain::product_checkpoint_test::product_checkpoint_domain_worker";
const PHASE: Duration = Duration::from_secs(45);
const START: Duration = Duration::from_secs(180);

/// The whole part of the one-minute load average plus one, clamped, or 1 where the host does not
/// publish one. Parsed textually: it multiplies a deadline, it is not a measurement.
fn observed_load() -> u32 {
    let Ok(text) = std::fs::read_to_string("/proc/loadavg") else {
        return 1;
    };
    text.split_ascii_whitespace()
        .next()
        .and_then(|value| value.split('.').next())
        .and_then(|whole| whole.parse::<u32>().ok())
        .map_or(1, |load| load.saturating_add(1).clamp(1, 16))
}

/// A budget for one phase of a journey, widened by the load the host is actually under.
///
/// Every wait these journeys perform is on a real signal -- a socket that accepts, a child that
/// exits, a file that grows -- so a longer budget cannot make a failing assertion pass; it can only
/// stop a still-progressing one being cut off because eighteen other tests were running. A fixed
/// budget measures the machine, and the machine is the part that moves.
fn budget(base: Duration) -> Duration {
    base * observed_load()
}
/// Close/reopen cycles driven against one workspace.
///
/// A single cycle passes on a tree where repeated cycling does not: the defect the user reported
/// only appears on iteration, so one cycle measures nothing about it.
const CYCLES: usize = 5;
const SCRIPT: &str = "\
guard=/tmp/husklet-continue-started; \
if test -e \"$guard\"; then echo FRESH_START_FORBIDDEN > /tmp/husklet-continue-fresh-start; exit 97; fi; \
echo initialized > \"$guard\"; \
sleep 1000 & a=$!; sleep 1000 & b=$!; sleep 1000 & c=$!; \
printf '%s %s %s %s\\n' \"$$\" \"$a\" \"$b\" \"$c\" > /tmp/husklet-continue-identities; \
while kill -0 \"$a\" && kill -0 \"$b\" && kill -0 \"$c\"; do \
  printf x >> /tmp/husklet-continue-progress; sleep .05; \
done; \
echo SLEEP_CHILD_LOST > /tmp/husklet-continue-failure; exit 91";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    temporary: Option<tempfile::TempDir>,
    home: PathBuf,
    workspace: WorkspaceConfig,
    domain: Domain,
    rootfs: PathBuf,
    helper_log: PathBuf,
    phases: Vec<String>,
}

struct DomainChild {
    child: Child,
    known: BTreeSet<u32>,
    armed: bool,
}

impl std::ops::Deref for DomainChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl std::ops::DerefMut for DomainChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for DomainChild {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let leader_running = self.child.try_wait().ok().flatten().is_none();
        if let Ok(current) = process_tree(self.child.id()) {
            self.known.extend(current);
        }
        // Kill every descendant observed while the leader still named its ownership tree.
        for pid in self.known.iter().copied().filter(|pid| *pid != self.child.id()) {
            let _ = crate::runtime::process::signal_for_test(pid, libc::SIGKILL);
        }
        // The helper owns a private process group; this catches children created after the last tree sample.
        let _ = crate::runtime::process::signal_group_for_test(self.child.id(), libc::SIGKILL);
        if leader_running {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = wait_processes_gone(&self.known, Duration::from_secs(5));
    }
}

impl Fixture {
    async fn create() -> TestResult<Self> {
        let temporary = context("create product checkpoint temporary directory", tempfile::tempdir())?;
        let prepared = async {
            let home = temporary.path().join("home");
            let storage = temporary.path().join("workspace");
            let rootfs = temporary.path().join("rootfs");
            context(
                format!("create fixture home {}", home.display()),
                std::fs::create_dir_all(&home),
            )?;
            context(
                format!("create workspace storage {}", storage.display()),
                std::fs::create_dir_all(&storage),
            )?;
            unpack_alpine(&rootfs)?;

            let guest = match std::env::var("HL_SCENARIO_TARGET") {
                Ok(value) if value == "amd64" => Guest::X86_64,
                Ok(value) if value == "arm64" => Guest::Aarch64,
                Err(std::env::VarError::NotPresent) => Guest::Aarch64,
                Ok(value) => return Err(format!("unsupported HL_SCENARIO_TARGET {value:?}").into()),
                Err(error) => return Err(format!("read HL_SCENARIO_TARGET: {error}").into()),
            };
            let arch = match guest {
                Guest::Aarch64 => hl_ws::Arch::Arm64,
                Guest::X86_64 => hl_ws::Arch::Amd64,
            };
            let mut workspace = WorkspaceConfig::new(
                format!("continue-product-{}", std::process::id()),
                "fixture:local",
                arch,
            );
            workspace.storage = Some(storage.clone());
            workspace.docker_sock = false;
            let store_path = home.join(".hl/workspaces.conf");
            let mut store = context(
                format!("open workspace store {}", store_path.display()),
                crate::config::WorkspaceStore::load(&store_path),
            )?;
            context(
                format!("publish workspace configuration {}", store_path.display()),
                store.upsert(workspace.clone()),
            )?;

            let checkpoints = std::sync::Arc::new(context(
                format!("open workspace checkpoint storage {}", storage.display()),
                crate::runtime::checkpoint::WorkspaceCheckpoints::open(&storage),
            )?);
            let container_storage = storage.join("containers");
            let containers = context(
                format!("open container repository {}", container_storage.display()),
                hl_container::Containers::builder(Config::new(container_storage.clone()))
                    .checkpoints(checkpoints)
                    .build()
                    .await,
            )?;
            let configuration = Configuration::new(&workspace);
            let signature = context("derive workspace signature", configuration.signature())?;
            let configuration_signature = context(
                "derive workspace configuration signature",
                configuration.identity_signature(),
            )?;
            let runtime_signature = configuration.runtime_signature();
            let session = context(
                format!("select workspace session from {}", rootfs.display()),
                crate::runtime::session::Session::from_root("", &rootfs),
            )?;
            let spec = ContainerSpec::from_directory(&rootfs, Process::new("/bin/sh").args(["-c", SCRIPT]))
                .name(CONTAINER)
                .guest(guest)
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    read_only_root: false,
                    network_isolated: true,
                    seccomp_baseline: hl_container::SeccompBaseline::Container,
                });
            let spec =
                session.label(configuration.container(spec, signature, configuration_signature, runtime_signature));
            let seeded = context("seed workspace primary container", containers.create(spec).await)?;
            context(
                "validate seeded workspace session authority",
                crate::runtime::session::Session::from_labels(&seeded.spec.labels),
            )?;
            context(
                "provision seeded workspace session",
                session.provision(&containers).await,
            )?;
            drop(containers);

            let domain = Domain::new(&workspace);
            let helper_log = temporary.path().join("domain-worker.log");
            Ok::<_, Box<dyn std::error::Error>>((home, workspace, domain, rootfs, helper_log))
        }
        .await;
        match prepared {
            Ok((home, workspace, domain, rootfs, helper_log)) => Ok(Self {
                temporary: Some(temporary),
                home,
                workspace,
                domain,
                rootfs,
                helper_log,
                phases: Vec::new(),
            }),
            Err(error) => {
                let artifact = temporary.keep();
                let _ = std::fs::write(artifact.join("FAILURE.txt"), format!("error={error}\n"));
                Err(format!("{error}; setup failure artifacts={}", artifact.display()).into())
            }
        }
    }

    fn spawn_domain(&mut self, cycle: usize) -> TestResult<DomainChild> {
        let output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.helper_log)?;
        let errors = output.try_clone()?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &self.workspace.name)
            .env(FORBID_REMOTE_ENV, "1")
            .env("HOME", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(errors));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            // Match the signed application's detached domain-worker boundary.
            command.process_group(0);
        }
        let started = Instant::now();
        let child = command.spawn()?;
        let mut child = DomainChild {
            known: BTreeSet::from([child.id()]),
            child,
            armed: true,
        };
        self.wait_domain(&mut child, budget(START))?;
        self.phase(format!("cycle={cycle} start_ms={}", started.elapsed().as_millis()));
        child.known.extend(process_tree(child.id())?);
        Ok(child)
    }

    fn wait_domain(&self, child: &mut Child, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if std::os::unix::net::UnixStream::connect(self.domain.socket()).is_ok()
                && PublishedProtocol::new(&self.domain.directory).compatible()?
            {
                return Ok(());
            }
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "domain worker exited before publication ({status}); helper={} domain={}",
                    self.helper_log.display(),
                    self.domain.directory.join("domain.log").display(),
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "domain worker publication timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn close_continue(&mut self, cycle: usize, child: &mut DomainChild) -> TestResult<BTreeSet<u32>> {
        let old_tree = process_tree(child.id())?;
        child.known.extend(old_tree.iter().copied());
        let started = Instant::now();
        if let Err(error) = self.domain.close_handover(Close::Continue, || Ok(())) {
            let load = observed_load();
            self.phase(format!(
                "cycle={cycle} close_failed_ms={} load={load} error={error}",
                started.elapsed().as_millis()
            ));
            // A refused capture under load is not a flake in this file and cannot be fixed here.
            // The engine's checkpoint coordinator gives the whole peer rendezvous a fixed wall
            // budget -- 500 passes of a 10 ms poll, `linux_abi/checkpoint/image.c` -- and refuses
            // the capture if any enumerated guest process has not reached a safepoint by then.
            // Starve the box and one of them will not. Say so, rather than leaving the next reader
            // to rediscover it from the domain log.
            if error.to_string().contains("CaptureRefused") {
                self.phase(format!(
                    "cycle={cycle} capture refused at load {load}: a participant did not reach a                      checkpoint safepoint inside the coordinator's fixed rendezvous budget. This is                      the engine's budget, not a deadline this test controls; see the domain worker                      log for the participant it named"
                ));
            }
            return Err(error.into());
        }
        if let Err(error) =
            wait_child(child, budget(PHASE)).and_then(|()| wait_processes_gone(&old_tree, budget(PHASE)))
        {
            self.phase(format!(
                "cycle={cycle} reap_failed_ms={} error={error}",
                started.elapsed().as_millis()
            ));
            return Err(error);
        }
        child.armed = false;
        self.phase(format!(
            "cycle={cycle} close_ms={} old_host_processes={}",
            started.elapsed().as_millis(),
            old_tree.len()
        ));
        Ok(old_tree)
    }

    fn phase(&mut self, value: String) {
        eprintln!("product-checkpoint {value}");
        self.phases.push(value);
    }

    fn preserve_failure(mut self, error: &dyn std::fmt::Display) {
        let root = self.temporary.take().expect("fixture root").keep();
        let mut report = format!("error={error}\n");
        for phase in &self.phases {
            report.push_str(phase);
            report.push('\n');
        }
        let _ = std::fs::write(root.join("FAILURE.txt"), report);
        eprintln!("product-checkpoint failure artifacts={}", root.display());
    }
}

#[test]
fn product_checkpoint_domain_worker() {
    let Some(name) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    // The signed application configures logging at its composition boundary; this worker is spawned
    // directly, so without this every broker-side refusal reason is dropped and the harness shows only
    // the engine's own stderr. A capture that refuses must be able to say why.
    crate::logging::configure();
    crate::runtime::worker::Worker::domain(&name.to_string_lossy()).expect("serve product checkpoint domain");
}

/// Covers the workspace **primary** process only: the `sleep` tree here belongs to the container's
/// own spec process, which the container's whole-image checkpoint restores.
///
/// It does **not** cover an execution session. A terminal pane runs as an exec, and an exec is
/// restored by `Executions::restore_checkpoints`, which this fixture never enters because it
/// creates no exec. A `sleep` typed into a pane therefore travels a path this test does not touch,
/// which is why the test is green while the product loses it.
#[tokio::test]
async fn continue_later_restores_the_primary_sleep_tree_across_repeated_cycles() {
    if std::env::var_os("HL_ALPINE_ARCHIVE").is_none() {
        assert!(
            std::env::var_os("HL_PRODUCT_CHECKPOINT_REQUIRED").is_none(),
            "normal product checkpoint gate requires HL_ALPINE_ARCHIVE"
        );
        eprintln!("product-checkpoint skipped: HL_ALPINE_ARCHIVE is not available on this host");
        return;
    }
    let mut fixture = match Fixture::create().await {
        Ok(fixture) => fixture,
        Err(error) => panic!("create product checkpoint fixture: {error}"),
    };
    fixture.phase(format!("load={}", observed_load()));
    let total = Instant::now();
    let outcome = run_cycles(&mut fixture);
    if let Err(error) = outcome {
        fixture.phase(format!("total_failed_ms={} error={error}", total.elapsed().as_millis()));
        fixture.preserve_failure(error.as_ref());
        panic!("product Continue-later checkpoint failed: {error}");
    }
    fixture.phase(format!("total_ms={}", total.elapsed().as_millis()));
}

fn run_cycles(fixture: &mut Fixture) -> TestResult {
    let progress = fixture.rootfs.join("tmp/husklet-continue-progress");
    let identities = fixture.rootfs.join("tmp/husklet-continue-identities");
    let fresh = fixture.rootfs.join("tmp/husklet-continue-fresh-start");
    let failure = fixture.rootfs.join("tmp/husklet-continue-failure");

    let mut domain = fixture.spawn_domain(0)?;
    wait_for_domain_growth(&progress, 0, budget(PHASE), &domain, &fixture.helper_log)?;
    let expected = read_guest_identities(&identities)?;
    assert_eq!(
        expected.len(),
        4,
        "fixture did not publish init plus three sleep identities"
    );
    for cycle in 1..=CYCLES {
        let _old_tree = fixture.close_continue(cycle, &mut domain)?;
        let stopped = std::fs::metadata(&progress)?.len();
        std::thread::sleep(Duration::from_millis(150));
        if std::fs::metadata(&progress)?.len() != stopped {
            return Err("guest progressed after the old domain lease was released".into());
        }
        domain = fixture.spawn_domain(cycle)?;
        wait_for_domain_growth(&progress, stopped, budget(PHASE), &domain, &fixture.helper_log)?;
        if fresh.exists() {
            return Err("restore silently fresh-started the primary process".into());
        }
        if failure.exists() {
            return Err("a restored sleep child was lost".into());
        }
        let restored = read_guest_identities(&identities)?;
        if restored != expected {
            return Err(
                format!("guest process identities changed: expected={expected:?} restored={restored:?}").into(),
            );
        }
    }
    let final_tree = process_tree(domain.id())?;
    domain.known.extend(final_tree.iter().copied());
    fixture.domain.close_handover(Close::Kill, || Ok(()))?;
    wait_child(&mut domain, budget(PHASE))?;
    wait_processes_gone(&final_tree, budget(PHASE))?;
    domain.armed = false;
    Ok(())
}

fn unpack_alpine(destination: &Path) -> TestResult {
    let archive = std::env::var_os("HL_ALPINE_ARCHIVE").ok_or("pinned Alpine archive is unavailable")?;
    context(
        format!("create Alpine rootfs {}", destination.display()),
        std::fs::create_dir(destination),
    )?;
    let archive_path = PathBuf::from(archive);
    let source = context(
        format!("open pinned Alpine archive {}", archive_path.display()),
        std::fs::File::open(&archive_path),
    )?;
    context(
        format!(
            "unpack pinned Alpine archive {} into {}",
            archive_path.display(),
            destination.display()
        ),
        tar::Archive::new(flate2::read::GzDecoder::new(source)).unpack(destination),
    )?;
    Ok(())
}

fn context<T, E>(operation: impl std::fmt::Display, result: Result<T, E>) -> TestResult<T>
where
    E: std::fmt::Display,
{
    result.map_err(|error| format!("{operation}: {error}").into())
}

fn read_guest_identities(path: &Path) -> TestResult<Vec<u32>> {
    std::fs::read_to_string(path)?
        .split_whitespace()
        .map(|value| value.parse::<u32>().map_err(Into::into))
        .collect()
}

fn wait_for_growth(path: &Path, previous: u64, timeout: Duration) -> TestResult {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > previous) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("{} did not progress beyond {previous} bytes", path.display()).into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_domain_growth(
    path: &Path,
    previous: u64,
    timeout: Duration,
    child: &DomainChild,
    helper_log: &Path,
) -> TestResult {
    wait_for_growth(path, previous, timeout).map_err(|error| {
        format!(
            "{error}; domain state at timeout:\n{}",
            domain_process_snapshot(child.id(), helper_log)
        )
        .into()
    })
}

fn domain_process_snapshot(root: u32, helper_log: &Path) -> String {
    let processes = Command::new("ps")
        .args([
            "-o",
            "pid=,ppid=,pgid=,sid=,stat=,wchan=,comm=",
            "--forest",
            "-g",
            &root.to_string(),
        ])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_else(|error| format!("ps failed: {error}"));
    let leader = std::fs::read_to_string(format!("/proc/{root}/status"))
        .unwrap_or_else(|error| format!("leader status unavailable: {error}"));
    let helper = std::fs::read_to_string(helper_log).unwrap_or_else(|error| format!("helper log unavailable: {error}"));
    format!("leader={root}\n{leader}\nprocesses:\n{processes}\nhelper log:\n{helper}")
}

fn wait_child(child: &mut Child, timeout: Duration) -> TestResult {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!("domain worker exited unsuccessfully: {status}").into());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("domain worker {} did not exit", child.id()).into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn process_tree(root: u32) -> TestResult<BTreeSet<u32>> {
    let output = Command::new("ps").args(["-axo", "pid=,ppid="]).output()?;
    if !output.status.success() {
        return Err("ps could not enumerate the domain process tree".into());
    }
    let mut children = BTreeMap::<u32, Vec<u32>>::new();
    for line in String::from_utf8(output.stdout)?.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some(parent) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        children.entry(parent).or_default().push(pid);
    }
    let mut tree = BTreeSet::from([root]);
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            if tree.insert(*child) {
                pending.push(*child);
            }
        }
    }
    Ok(tree)
}

fn wait_processes_gone(processes: &BTreeSet<u32>, timeout: Duration) -> TestResult {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = processes
            .iter()
            .copied()
            .filter(|pid| crate::runtime::process::process_exists_for_test(*pid).unwrap_or(true))
            .collect::<Vec<_>>();
        if remaining.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("old domain host processes survived handover: {remaining:?}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// Terminal-backed exec journey
// ---------------------------------------------------------------------------

/// Close/reopen cycles driven against one **terminal-backed execution session**.
///
/// The fixture above covers the container's own spec process. A terminal pane is not that: it is an
/// exec created with `tty: true` and a `console_size`, and it is restored by a different path. Every
/// exec in the container suite is pipe-backed and this file's other journey creates no exec at all,
/// so nothing else in the repository drives the sequence a user drives -- open, run a command at the
/// pane's prompt, Continue later, reopen -- and nothing else enters the exec reattach path.
const EXEC_MARKER: &str = "while :; do printf x >> /tmp/husklet-journey-progress; sleep .05; done &\n";
/// The interactive shell a pane runs, minus the product's shell selection.
const EXEC_SHELL: &str = "cd /root; exec /bin/sh -i";
/// Fewer cycles than the primary journey: each one carries a whole-image capture plus an exec.
const EXEC_CYCLES: usize = 3;

/// What a reopened workspace was able to do with the pane's persisted execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Resumption {
    /// The execution came back running and the pane reattached to it.
    Reattached,
    /// The execution came back but the domain refused to hand it over. The pane must say so.
    Refused,
    /// The execution record did not survive the capture at all. Production answers a 404 by
    /// creating a second shell, which is what makes a lost command look like a live prompt.
    Discarded,
}

struct ExecJourney {
    socket: PathBuf,
    client: hl_client::Client,
    execution: String,
    config: hl_client::model::ExecConfig,
    start: hl_client::model::ExecStart,
    attach: hl_client::model::ExecAttach,
}

impl ExecJourney {
    /// Builds the exec exactly as `runtime::execution::launch` does for a pane.
    async fn create(socket: PathBuf) -> TestResult<Self> {
        use hl_client::model::{Attachment, ExecAttach, ExecConfig, ExecStart};

        let client = context("connect workspace domain", hl_client::Client::unix(socket.clone()))?;
        context(
            "start workspace container for the pane execution",
            match client.containers().start(CONTAINER).await {
                Err(hl_client::Error::Docker { status, .. }) if matches!(status.as_u16(), 304 | 409) => Ok(()),
                other => other,
            },
        )?;
        let console = Some([24, 80]);
        let config = ExecConfig {
            attach: Attachment {
                stdin: true,
                stdout: true,
                stderr: true,
            },
            tty: true,
            env: Some(vec!["HOME=/root".to_owned()]),
            command: vec!["/bin/sh".into(), "-c".into(), EXEC_SHELL.into()],
            user: "0:0".into(),
            working_dir: "/root".into(),
            ..ExecConfig::default()
        };
        let start = ExecStart {
            tty: true,
            kill_on_disconnect: true,
            console_size: console,
            ..ExecStart::default()
        };
        let attach = ExecAttach {
            tty: true,
            kill_on_disconnect: true,
            console_size: console,
        };
        let created = context(
            "create terminal-backed pane execution",
            client.executions().create(CONTAINER, &config).await,
        )?;
        Ok(Self {
            socket,
            client,
            execution: created.id,
            config,
            start,
            attach,
        })
    }

    /// Starts the pane's shell and types the long-running command a user types at its prompt.
    async fn open_and_type(&self) -> TestResult<hl_client::api::TerminalOutput> {
        let session = context(
            "start terminal-backed pane execution",
            self.client.executions().start(&self.execution, &self.start).await,
        )?;
        let (mut input, output) = context("split pane execution terminal", session.into_terminal())?;
        context(
            "type the pane's long-running command",
            input.write(EXEC_MARKER.as_bytes()).await,
        )?;
        Ok(output)
    }

    /// Resumes the pane's execution exactly as a reopened workspace does: a running execution is
    /// reattached, and a refusal is reported rather than replaced by a second shell.
    ///
    /// This lane never relaunches a restored execution. A `Refused` outcome is a finding to report,
    /// not a state to paper over by creating a new one.
    async fn resume(&mut self) -> TestResult<(Resumption, Option<hl_client::api::TerminalOutput>)> {
        // A reopened workspace is served by a **new** domain process, so a pane connects afresh --
        // the previous connection belongs to a domain that has already gone away.
        self.client = context(
            "reconnect reopened workspace domain",
            hl_client::Client::unix(self.socket.clone()),
        )?;
        context(
            "start workspace container after reopen",
            match self.client.containers().start(CONTAINER).await {
                Err(hl_client::Error::Docker { status, .. }) if matches!(status.as_u16(), 304 | 409) => Ok(()),
                other => other,
            },
        )?;
        let inspection = match self.client.executions().inspect(&self.execution).await {
            Ok(inspection) => inspection,
            Err(hl_client::Error::Docker { status, .. }) if status.as_u16() == 404 => {
                return Ok((Resumption::Discarded, None))
            }
            Err(error) => return Err(format!("inspect the persisted pane execution after reopen: {error}").into()),
        };
        if !inspection.running {
            return Ok((Resumption::Refused, None));
        }
        match self.client.executions().attach(&self.execution, &self.attach).await {
            Ok(session) => {
                let (_input, output) = context("split reattached pane terminal", session.into_terminal())?;
                Ok((Resumption::Reattached, Some(output)))
            }
            Err(error) => {
                eprintln!("product-checkpoint exec reattach refused: {error}");
                Ok((Resumption::Refused, None))
            }
        }
    }

    /// The pane's persisted execution identity must never be silently replaced.
    fn identity(&self) -> &str {
        &self.execution
    }

    fn configured_command(&self) -> &[String] {
        &self.config.command
    }
}

/// Drives open -> exec -> Continue later -> reopen, N times, against a terminal-backed pane.
#[tokio::test]
async fn continue_later_keeps_a_terminal_backed_pane_execution_across_repeated_cycles() {
    // Unconditional. This was opt-in behind HL_PRODUCT_EXEC_JOURNEY while a pane's terminal-backed
    // execution did not survive the capture; the transport defect that lost it is fixed and all three
    // cycles now come back Reattached, so the journey is a gate rather than a reproduction.
    if std::env::var_os("HL_ALPINE_ARCHIVE").is_none() {
        assert!(
            std::env::var_os("HL_PRODUCT_CHECKPOINT_REQUIRED").is_none(),
            "normal product checkpoint gate requires HL_ALPINE_ARCHIVE"
        );
        eprintln!("product-checkpoint exec journey skipped: HL_ALPINE_ARCHIVE is not available on this host");
        return;
    }
    let mut fixture = match Fixture::create().await {
        Ok(fixture) => fixture,
        Err(error) => panic!("create product checkpoint fixture: {error}"),
    };
    fixture.phase(format!("exec load={}", observed_load()));
    let total = Instant::now();
    let outcome = run_exec_cycles(&mut fixture).await;
    if let Err(error) = outcome {
        fixture.phase(format!(
            "exec total_failed_ms={} error={error}",
            total.elapsed().as_millis()
        ));
        fixture.preserve_failure(error.as_ref());
        panic!("product Continue-later exec journey failed: {error}");
    }
    fixture.phase(format!("exec total_ms={}", total.elapsed().as_millis()));
}

async fn run_exec_cycles(fixture: &mut Fixture) -> TestResult {
    let progress = fixture.rootfs.join("tmp/husklet-journey-progress");
    let _ = std::fs::remove_file(&progress);

    let mut domain = fixture.spawn_domain(0)?;
    let mut journey = ExecJourney::create(fixture.domain.socket()).await?;
    let identity = journey.identity().to_owned();
    let command = journey.configured_command().to_vec();
    let mut attachment = Some(journey.open_and_type().await?);
    wait_for_growth(&progress, 0, budget(PHASE))?;
    fixture.phase(format!("exec cycle=0 execution={identity} typed"));

    for cycle in 1..=EXEC_CYCLES {
        // The application closes pane attachments only after the capture is requested, so hold the
        // terminal across the close exactly as a pane does and release it afterwards.
        let old_tree = fixture.close_continue(cycle, &mut domain)?;
        drop(attachment.take());
        let stopped = std::fs::metadata(&progress)?.len();
        std::thread::sleep(Duration::from_millis(150));
        if std::fs::metadata(&progress)?.len() != stopped {
            return Err("the pane's command progressed after the old domain lease was released".into());
        }
        domain = fixture.spawn_domain(cycle)?;
        domain.known.extend(old_tree);

        let (resumption, output) = journey.resume().await?;
        fixture.phase(format!("exec cycle={cycle} resumption={resumption:?}"));
        // Whatever the domain decides, it must decide it about the *same* execution: a pane that
        // silently acquired a second shell is the defect the user reported.
        assert_eq!(
            journey.identity(),
            identity,
            "the pane's persisted execution identity must survive a Continue-later cycle"
        );
        assert_eq!(
            journey.configured_command(),
            command.as_slice(),
            "the resumed execution must be the terminal-backed shell the pane created"
        );
        match resumption {
            Resumption::Reattached => {
                attachment = output;
                wait_for_growth(&progress, stopped, budget(PHASE))?;
            }
            Resumption::Refused => {
                return Err(format!(
                    "cycle {cycle}: the domain refused to resume the pane's terminal-backed execution \
                     {identity}; the pane must report that refusal rather than open a second shell"
                )
                .into());
            }
            Resumption::Discarded => {
                return Err(format!(
                    "cycle {cycle}: the pane's terminal-backed execution {identity} no longer exists after \
                     the Continue-later capture (404). The command typed at the pane's prompt is gone, and \
                     the product answers a 404 by creating a second shell -- which is why a lost session \
                     presents to the user as a working prompt"
                )
                .into());
            }
        }
    }

    let final_tree = process_tree(domain.id())?;
    domain.known.extend(final_tree.iter().copied());
    drop(attachment.take());
    fixture.domain.close_handover(Close::Kill, || Ok(()))?;
    wait_child(&mut domain, budget(PHASE))?;
    wait_processes_gone(&final_tree, budget(PHASE))?;
    domain.armed = false;
    Ok(())
}
