//! Product-path checkpoint acceptance for the workspace domain.

use super::*;
use hl_container::{Config, ContainerSpec, Guest, Isolation, Process, Sandbox};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "HL_PRODUCT_CHECKPOINT_DOMAIN_CHILD";
const CHILD_TEST: &str = "runtime::domain::product_checkpoint_test::product_checkpoint_domain_worker";
const PHASE: Duration = Duration::from_secs(45);
const START: Duration = Duration::from_secs(180);
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
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        // The helper owns a private process group; this catches children created after the last tree sample.
        let _ = unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
        if leader_running {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = wait_processes_gone(&self.known, Duration::from_secs(5));
    }
}

impl Fixture {
    async fn create() -> TestResult<Self> {
        let temporary = tempfile::tempdir()?;
        let prepared = async {
            let home = temporary.path().join("home");
            let storage = temporary.path().join("workspace");
            let rootfs = temporary.path().join("rootfs");
            std::fs::create_dir_all(&home)?;
            unpack_alpine(&rootfs)?;

            let guest = match std::env::var("HL_SCENARIO_TARGET") {
                Ok(value) if value == "amd64" => Guest::X86_64,
                Ok(value) if value == "arm64" => Guest::Aarch64,
                Err(std::env::VarError::NotPresent) => Guest::Aarch64,
                Ok(value) => return Err(format!("unsupported HL_SCENARIO_TARGET {value:?}").into()),
                Err(error) => return Err(error.into()),
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
            let mut store = crate::config::WorkspaceStore::load(&store_path)?;
            store.upsert(workspace.clone())?;

            let checkpoints = std::sync::Arc::new(
                crate::runtime::checkpoint::WorkspaceCheckpoints::open(&storage).map_err(io::Error::other)?,
            );
            let containers = hl_container::Containers::builder(Config::new(storage.join("containers")))
                .checkpoints(checkpoints)
                .build()
                .await?;
            let configuration = Configuration::new(&workspace);
            let signature = configuration.signature()?;
            let configuration_signature = configuration.configuration_signature()?;
            let runtime_signature = configuration.runtime_signature();
            let spec = ContainerSpec::from_directory(&rootfs, Process::new("/bin/sh").args(["-c", SCRIPT]))
                .name(CONTAINER)
                .guest(guest)
                .isolation(Isolation {
                    sandbox: Sandbox::Disabled,
                    read_only_root: false,
                    network_isolated: true,
                    seccomp_baseline: hl_container::SeccompBaseline::Container,
                });
            containers
                .create(configuration.container(spec, signature, configuration_signature, runtime_signature))
                .await?;
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
        self.wait_domain(&mut child, START)?;
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
            self.phase(format!(
                "cycle={cycle} close_failed_ms={} error={error}",
                started.elapsed().as_millis()
            ));
            return Err(error.into());
        }
        if let Err(error) = wait_child(child, PHASE).and_then(|()| wait_processes_gone(&old_tree, PHASE)) {
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
    crate::runtime::worker::Worker::domain(&name.to_string_lossy()).expect("serve product checkpoint domain");
}

#[tokio::test]
async fn continue_later_restores_sleep_tree_in_two_fresh_domain_processes() {
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
    let total = Instant::now();
    let outcome = run_cycles(&mut fixture).await;
    if let Err(error) = outcome {
        fixture.phase(format!("total_failed_ms={} error={error}", total.elapsed().as_millis()));
        fixture.preserve_failure(error.as_ref());
        panic!("product Continue-later checkpoint failed: {error}");
    }
    fixture.phase(format!("total_ms={}", total.elapsed().as_millis()));
}

async fn run_cycles(fixture: &mut Fixture) -> TestResult {
    let progress = fixture.rootfs.join("tmp/husklet-continue-progress");
    let identities = fixture.rootfs.join("tmp/husklet-continue-identities");
    let fresh = fixture.rootfs.join("tmp/husklet-continue-fresh-start");
    let failure = fixture.rootfs.join("tmp/husklet-continue-failure");

    let mut domain = fixture.spawn_domain(0)?;
    wait_for_growth(&progress, 0, PHASE)?;
    let expected = read_guest_identities(&identities)?;
    assert_eq!(
        expected.len(),
        4,
        "fixture did not publish init plus three sleep identities"
    );
    for cycle in 1..=2 {
        let _old_tree = fixture.close_continue(cycle, &mut domain)?;
        let stopped = std::fs::metadata(&progress)?.len();
        std::thread::sleep(Duration::from_millis(150));
        if std::fs::metadata(&progress)?.len() != stopped {
            return Err("guest progressed after the old domain lease was released".into());
        }
        domain = fixture.spawn_domain(cycle)?;
        wait_for_growth(&progress, stopped, PHASE)?;
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
    wait_child(&mut domain, PHASE)?;
    wait_processes_gone(&final_tree, PHASE)?;
    domain.armed = false;
    Ok(())
}

fn unpack_alpine(destination: &Path) -> TestResult {
    let archive = std::env::var_os("HL_ALPINE_ARCHIVE").ok_or("pinned Alpine archive is unavailable")?;
    std::fs::create_dir(destination)?;
    tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?)).unpack(destination)?;
    Ok(())
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
            .filter(|pid| unsafe { libc::kill(*pid as libc::pid_t, 0) } == 0)
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
