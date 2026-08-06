use std::collections::{BTreeMap, VecDeque};
mod engine_binary;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[path = "inventory/resume.rs"]
mod resume;
#[path = "inventory/supervision.rs"]
mod supervision;

const REPORT_HEADER: &str = "suite\tcase\tisa\tstatus\texit\tdependencies\tmismatch\tdiagnostic\n";

static SCRATCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct Case {
    suite: String,
    name: String,
    isa: String,
    artifact: PathBuf,
    exit: i32,
    stdout: Option<PathBuf>,
    stderr: Option<PathBuf>,
    timeout: u64,
    environment: String,
    dependencies: String,
    skip: Option<String>,
    fixture: String,
    arguments: String,
    side_files: String,
    rootfs: String,
    guest_executable: String,
}

struct Setup {
    fixture: String,
    arguments: String,
    side_files: String,
    rootfs: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Host {
    Linux,
    Macos,
    Windows,
}

impl Host {
    fn current() -> Self {
        #[cfg(target_os = "linux")]
        return Self::Linux;
        #[cfg(target_os = "macos")]
        return Self::Macos;
        #[cfg(target_os = "windows")]
        return Self::Windows;
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        panic!("unsupported compatibility host");
    }

    fn accepts(self, disposition: &str) -> bool {
        match disposition {
            "active" => true,
            "excluded-macos" => self != Self::Macos,
            "excluded-windows" => self != Self::Windows,
            value if value.starts_with("excluded-") => false,
            value => panic!("unsupported compatibility disposition: {value}"),
        }
    }
}

struct ResultRow {
    case: Case,
    status: &'static str,
    exit: String,
    diagnostic: String,
    mismatch: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimeoutProfile {
    Compatibility,
    Performance,
}

impl TimeoutProfile {
    const PERFORMANCE_MS: u64 = 10_000;

    fn settings() -> Self {
        match setting("HL_COMPAT_TIMEOUT_PROFILE").as_deref() {
            None | Some("compatibility") => Self::Compatibility,
            Some("performance") => Self::Performance,
            Some(_) => panic!("HL_COMPAT_TIMEOUT_PROFILE must be compatibility or performance"),
        }
    }

    fn deadline(self, inventory_ms: u64) -> u64 {
        match self {
            Self::Compatibility => inventory_ms,
            Self::Performance => inventory_ms.min(Self::PERFORMANCE_MS),
        }
    }
}

impl Case {
    fn key(&self) -> (String, String, String) {
        (self.suite.clone(), self.name.clone(), self.isa.clone())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Selection {
    suite: Option<String>,
    isa: Option<String>,
    name: Option<String>,
}

impl Selection {
    fn settings() -> Self {
        Self {
            suite: setting("HL_COMPAT_SUITE"),
            isa: setting("HL_COMPAT_ISA"),
            name: setting("HL_COMPAT_CASE"),
        }
    }

    fn apply(&self, cases: Vec<Case>) -> Vec<Case> {
        cases
            .into_iter()
            .filter(|case| {
                self.suite.as_ref().is_none_or(|value| &case.suite == value)
                    && self.isa.as_ref().is_none_or(|value| &case.isa == value)
                    && self.name.as_ref().is_none_or(|value| case.name.contains(value))
            })
            .collect()
    }

    fn filtered(&self) -> bool {
        self.suite.is_some() || self.isa.is_some() || self.name.is_some()
    }

    fn component(label: &str, value: &str) -> String {
        let mut encoded = String::from(label);
        encoded.push('-');
        for byte in value.bytes().take(64) {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                encoded.push(byte as char);
            } else {
                encoded.push_str(&format!("_{byte:02x}"));
            }
        }
        encoded
    }
}

struct ReportTarget(PathBuf);

impl ReportTarget {
    fn resolve(root: &Path, selection: &Selection, explicit: Option<String>) -> Self {
        if let Some(path) = explicit {
            return Self(PathBuf::from(path));
        }
        if !selection.filtered() {
            return Self(root.join("report/api-results.tsv"));
        }
        let mut parts = Vec::new();
        if let Some(value) = &selection.suite {
            parts.push(Selection::component("suite", value));
        }
        if let Some(value) = &selection.isa {
            parts.push(Selection::component("isa", value));
        }
        if let Some(value) = &selection.name {
            parts.push(Selection::component("case", value));
        }
        Self(
            root.join("report")
                .join(format!("api-results--{}.tsv", parts.join("--"))),
        )
    }

    fn write(&self, rows: &[ResultRow]) {
        let mut output = String::from(REPORT_HEADER);
        for row in rows {
            output.push_str(&format_row(row));
        }
        if let Some(parent) = self.0.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&self.0, output).unwrap();
        let mut counts = BTreeMap::new();
        for row in rows.iter().filter(|row| row.status == "fail" && row.mismatch != "-") {
            for class in mismatch_classes(&row.mismatch) {
                *counts.entry((class, row.case.suite.as_str())).or_insert(0_usize) += 1;
            }
        }
        let mut summary = String::from("mismatch\tsuite\tcases\n");
        for ((class, suite), cases) in counts {
            summary.push_str(&format!("{class}\t{suite}\t{cases}\n"));
        }
        fs::write(self.0.with_extension("summary.tsv"), summary).unwrap();
    }
}

fn mismatch_classes(value: &str) -> Vec<&'static str> {
    let mut classes = Vec::new();
    if value.contains("exit:") {
        classes.push("expected-exit");
    }
    if value.contains("stdout:") {
        classes.push("stdout");
    }
    if value.contains("stderr:") {
        classes.push("stderr");
    }
    if value.contains("engine:") {
        classes.push("cleanup-engine-exit");
    }
    classes
}

#[test]
#[ignore = "full imported compatibility inventory"]
fn inventory_matrix() {
    #[cfg(unix)]
    let _signals =
        hl_engine::native::TerminationSignals::install().expect("install compatibility termination handlers");
    let root = corpus();
    let selection = Selection::settings();
    let timeout_profile = TimeoutProfile::settings();
    let report = ReportTarget::resolve(&root, &selection, setting("HL_COMPAT_REPORT"));
    let engine_options = setting("HL_COMPAT_ENGINE_OPTIONS");
    worker_environment("-", engine_options.as_deref()).expect("valid HL_COMPAT_ENGINE_OPTIONS");
    let cases = selection.apply(parse(&root, timeout_profile));
    assert!(!cases.is_empty(), "inventory filter selected no rows");
    let total = cases.len();
    let stamp = run_stamp(&root, &selection, &cases, timeout_profile).expect("fingerprint compatibility inputs");
    let resumable = switch("HL_COMPAT_RESUME");
    let mut run = resume::open(&report.0, &stamp, cases, resumable).unwrap();
    let batch = positive("HL_COMPAT_BATCH");
    assert!(
        batch.is_none() || resumable,
        "HL_COMPAT_BATCH requires HL_COMPAT_RESUME=1"
    );
    if let Some(limit) = batch {
        run.pending.truncate(limit);
    }
    let deferred = total - run.prior.len() - run.pending.len();
    let jobs = setting("HL_COMPAT_JOBS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_JOBS)
        .clamp(1, 32);
    let queue = Arc::new(Mutex::new(VecDeque::from(run.pending)));
    let completed_count = run.prior.len() as u64;
    let results = Arc::new(Mutex::new(run.prior));
    let completed = Arc::new(AtomicU64::new(completed_count));
    let stop = Arc::new(AtomicBool::new(false));
    let ledger = Arc::new(run.ledger);
    let engine_options = Arc::new(engine_options);
    let mut workers = Vec::new();
    for _ in 0..jobs {
        workers.push(spawn(
            Arc::clone(&queue),
            Arc::clone(&results),
            Arc::clone(&completed),
            Arc::clone(&ledger),
            Arc::clone(&stop),
            Arc::clone(&engine_options),
            total as u64,
        ));
    }
    for worker in workers {
        if worker.join().is_err() {
            stop.store(true, Ordering::Release);
        }
    }
    let remaining = deferred + queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len();
    let mut results = std::mem::take(&mut *results.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    results.sort_by(|a, b| (&a.case.isa, &a.case.suite, &a.case.name).cmp(&(&b.case.isa, &b.case.suite, &b.case.name)));
    if remaining == 0 {
        report.write(&results);
        ledger.finish().expect("finalize compatibility ledger");
    } else {
        println!(
            "compat partial={} remaining={remaining}; rerun with HL_COMPAT_RESUME=1",
            results.len(),
        );
    }
    let failed = results.iter().filter(|row| row.status == "fail").count();
    let skipped = results.iter().filter(|row| row.status == "skip").count();
    println!(
        "compat completed={} total={total} failed={failed} skipped={skipped}",
        results.len()
    );
    assert_eq!(
        failed,
        0,
        "compatibility failures; see {}",
        if remaining == 0 {
            report.0.display().to_string()
        } else {
            report.0.with_extension("partial.tsv").display().to_string()
        },
    );
}

fn spawn(
    queue: Arc<Mutex<VecDeque<Case>>>,
    results: Arc<Mutex<Vec<ResultRow>>>,
    completed: Arc<AtomicU64>,
    ledger: Arc<resume::Ledger>,
    stop: Arc<AtomicBool>,
    engine_options: Arc<Option<String>>,
    total: u64,
) -> thread::JoinHandle<()> {
    thread::spawn(move || work(queue, results, completed, ledger, stop, engine_options, total))
}

fn work(
    queue: Arc<Mutex<VecDeque<Case>>>,
    results: Arc<Mutex<Vec<ResultRow>>>,
    completed: Arc<AtomicU64>,
    ledger: Arc<resume::Ledger>,
    stop: Arc<AtomicBool>,
    engine_options: Arc<Option<String>>,
    total: u64,
) {
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let case = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front();
        let Some(case) = case else { break };
        let panic_case = case.clone();
        let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(case, engine_options.as_deref())
        }))
        .unwrap_or_else(|_| {
            harness(
                panic_case,
                "worker-panic",
                io::Error::other("compatibility worker panicked"),
            )
        });
        if let Err(error) = ledger.record(&result) {
            stop.store(true, Ordering::Release);
            result.status = "fail";
            result.diagnostic = format!("ledger={error}");
            result.mismatch = "engine:harness-ledger".into();
        }
        results.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(result);
        let count = completed.fetch_add(1, Ordering::Relaxed) + 1;
        progress(count, total);
    }
}

fn progress(count: u64, total: u64) {
    if count == total || count.is_multiple_of(100) {
        eprintln!("compat progress={count}/{total}");
    }
}

fn execute(case: Case, engine_options: Option<&str>) -> ResultRow {
    if let Some(reason) = &case.skip {
        return row(case.clone(), "skip", "-", reason, "-");
    }
    let scratch = std::env::temp_dir().join(format!(
        "hl-inventory-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed),
    ));
    if let Err(error) = fs::create_dir_all(&scratch) {
        return harness(case, "scratch", error);
    }
    let report = scratch.join("result");
    let output = match fs::File::create(scratch.join("stdout")) {
        Ok(output) => output,
        Err(error) => return harness_cleanup(case, "capture-output", error, &scratch),
    };
    let error = match fs::File::create(scratch.join("stderr")) {
        Ok(error) => error,
        Err(error) => return harness_cleanup(case, "capture-error", error, &scratch),
    };
    let worker = env!("CARGO_BIN_EXE_hl-compat-worker");
    let environment = match worker_environment(&case.environment, engine_options) {
        Ok(environment) => environment,
        Err(error) => return harness_cleanup(case, "engine-options", io::Error::other(error), &scratch),
    };
    let mut command = Command::new(worker);
    supervision::configure(&mut command);
    command
        .arg(&case.isa)
        .arg(&case.artifact)
        .arg(&environment)
        .arg(&report)
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error))
        .arg(&case.fixture)
        .arg(&case.arguments)
        .arg(&case.side_files)
        .arg(&case.rootfs)
        .arg(&case.guest_executable);
    if setting("HL_COMPAT_TRACE").is_some() {
        command.arg("trace");
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return harness_cleanup(case, "spawn", error, &scratch),
    };
    let wall = Duration::from_millis(case.timeout);
    let outcome = match supervision::wait(
        &mut child,
        wall,
        supervision::stall_budget(wall),
        &scratch.join("stdout"),
        &scratch.join("stderr"),
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let containment = supervision::contain(&mut child)
                .err()
                .map_or_else(|| "contained".to_owned(), |failure| format!("containment={failure}"));
            let _ = fs::remove_dir_all(&scratch);
            return row(
                case,
                "fail",
                "-",
                &format!("wait={error}; {containment}"),
                "engine:harness-wait",
            );
        }
    };
    let status = match outcome {
        supervision::Outcome::Exited(status) => status,
        supervision::Outcome::Interrupted(signal) => {
            let _ = fs::remove_dir_all(&scratch);
            return row(
                case,
                "fail",
                "-",
                &format!("interrupted by signal {signal}"),
                "engine:harness-interrupted",
            );
        }
        supervision::Outcome::OutputLimit => {
            let _ = fs::remove_dir_all(&scratch);
            return row(case, "fail", "-", "capture limit", "engine:output-limit");
        }
        supervision::Outcome::Stalled => {
            let _ = fs::remove_dir_all(&scratch);
            return row(case, "fail", "-", "stall", "engine:stall");
        }
        supervision::Outcome::TimedOut(evidence) => {
            let _ = fs::remove_dir_all(&scratch);
            return row(case, "fail", "-", &evidence.to_string(), "engine:timeout");
        }
    };
    let text = fs::read_to_string(&report).unwrap_or_default();
    let mut fields = text.lines();
    let exit = fields.next().unwrap_or("-").to_owned();
    let cleaned = fields.next() == Some("true");
    let diagnostic = fields.collect::<Vec<_>>().join(" | ");
    let diagnostic = if diagnostic.is_empty() {
        "worker-error".into()
    } else {
        diagnostic
    };
    let stdout = match fs::read(scratch.join("stdout")) {
        Ok(stdout) => stdout,
        Err(error) => return harness_cleanup(case, "read-output", error, &scratch),
    };
    let stderr = match fs::read(scratch.join("stderr")) {
        Ok(stderr) => stderr,
        Err(error) => return harness_cleanup(case, "read-error", error, &scratch),
    };
    let expected_output = match golden(case.stdout.as_deref()) {
        Ok(output) => output,
        Err(error) => return harness_cleanup(case, "golden-output", error, &scratch),
    };
    let expected_error = match case.stderr.as_deref().map(fs::read).transpose() {
        Ok(error) => error,
        Err(error) => return harness_cleanup(case, "golden-error", error, &scratch),
    };
    let output_ok = expected_output == stdout;
    let error_ok = expected_error.as_ref().is_none_or(|expected| expected == &stderr);
    let worker_ok = status.success();
    let exit_ok = exit.parse() == Ok(case.exit);
    let mismatch = mismatch(
        worker_ok,
        cleaned,
        exit_ok,
        case.exit,
        &exit,
        &expected_output,
        &stdout,
        expected_error.as_deref(),
        &stderr,
    );
    let passed = worker_ok && exit_ok && cleaned && output_ok && error_ok;
    let mut result = row(
        case,
        if passed { "pass" } else { "fail" },
        &exit,
        &diagnostic,
        &mismatch,
    );
    if let Err(error) = fs::remove_dir_all(&scratch) {
        result.status = "fail";
        result.diagnostic = format!("cleanup={error}");
        result.mismatch = "engine:harness-cleanup".into();
    }
    result
}

fn golden(path: Option<&Path>) -> io::Result<Vec<u8>> {
    path.map_or_else(|| Ok(Vec::new()), fs::read)
}

fn harness(case: Case, stage: &str, error: io::Error) -> ResultRow {
    row(
        case,
        "fail",
        "-",
        &format!("{stage}={error}"),
        &format!("engine:harness-{stage}"),
    )
}

fn harness_cleanup(case: Case, stage: &str, error: io::Error, scratch: &Path) -> ResultRow {
    let cleanup = fs::remove_dir_all(scratch)
        .err()
        .map(|failure| format!("; cleanup={failure}"))
        .unwrap_or_default();
    row(
        case,
        "fail",
        "-",
        &format!("{stage}={error}{cleanup}"),
        &format!("engine:harness-{stage}"),
    )
}

fn mismatch(
    worker_ok: bool,
    cleaned: bool,
    exit_ok: bool,
    expected_exit: i32,
    actual_exit: &str,
    expected_output: &[u8],
    actual_output: &[u8],
    expected_error: Option<&[u8]>,
    actual_error: &[u8],
) -> String {
    let mut parts = Vec::new();
    if !exit_ok {
        parts.push(format!("exit:e={expected_exit},a={actual_exit}"));
    }
    if expected_output != actual_output {
        parts.push(difference("stdout", expected_output, actual_output));
    }
    if let Some(expected_error) = expected_error.filter(|expected| *expected != actual_error) {
        parts.push(difference("stderr", expected_error, actual_error));
    }
    if !worker_ok || !cleaned {
        parts.push(format!("engine:worker={},cleaned={cleaned}", u8::from(worker_ok)));
    }
    if parts.is_empty() { "-".into() } else { parts.join(";") }
}

fn difference(label: &str, expected: &[u8], actual: &[u8]) -> String {
    let first = expected
        .iter()
        .zip(actual)
        .position(|(left, right)| left != right)
        .unwrap_or(expected.len().min(actual.len()));
    let expected_byte = expected.get(first).map_or("--".into(), |byte| format!("{byte:02x}"));
    let actual_byte = actual.get(first).map_or("--".into(), |byte| format!("{byte:02x}"));
    format!(
        "{label}:e={:016x},a={:016x},d={first},eb={expected_byte},ab={actual_byte},el={},al={}",
        digest(expected),
        digest(actual),
        expected.len(),
        actual.len()
    )
}

fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn row(case: Case, status: &'static str, exit: &str, diagnostic: &str, mismatch: &str) -> ResultRow {
    ResultRow {
        case,
        status,
        exit: exit.to_owned(),
        diagnostic: diagnostic.replace('\t', " "),
        mismatch: mismatch.replace('\t', " "),
    }
}

fn format_row(row: &ResultRow) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        row.case.suite,
        row.case.name,
        row.case.isa,
        row.status,
        row.exit,
        row.case.dependencies,
        row.mismatch,
        row.diagnostic,
    )
}

fn parse(root: &Path, timeout_profile: TimeoutProfile) -> Vec<Case> {
    let setup = setup(root);
    let executables = guest_executables(root);
    let text = fs::read_to_string(root.join("inventory.tsv")).unwrap();
    let host = Host::current();
    text.lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let value = line.split('\t').collect::<Vec<_>>();
            assert_eq!(value.len(), 13, "unsupported inventory schema");
            if !host.accepts(value[11]) {
                return None;
            }
            let dependencies = value[9];
            let key = (value[0].to_owned(), value[1].to_owned(), value[2].to_owned());
            let fixture = setup.get(&key);
            let guest_executable = executables.get(&key).cloned().unwrap_or_else(|| {
                Path::new("/")
                    .join(Path::new(value[3]).file_name().expect("artifact has a leaf"))
                    .to_string_lossy()
                    .into_owned()
            });
            let skip = fixture.and_then(unsupported);
            let rootfs = fixture.map_or_else(|| "-".to_owned(), |value| rootfs_argument(root, &value.rootfs));
            Some(Case {
                suite: value[0].into(),
                name: value[1].into(),
                isa: value[2].into(),
                artifact: root.join(value[3]),
                exit: value[4].parse().unwrap(),
                stdout: path(root, value[5]),
                stderr: path(root, value[6]),
                timeout: timeout_profile.deadline(value[7].parse().unwrap()),
                environment: value[10].into(),
                dependencies: dependencies.into(),
                skip,
                fixture: fixture.map_or("executable", |value| &value.fixture).into(),
                arguments: fixture.map_or("-", |value| &value.arguments).into(),
                side_files: fixture.map_or("-", |value| &value.side_files).into(),
                rootfs,
                guest_executable,
            })
        })
        .collect()
}

fn guest_executables(root: &Path) -> BTreeMap<(String, String, String), String> {
    fs::read_to_string(root.join("build-plan.tsv"))
        .unwrap()
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let value = line.split('\t').collect::<Vec<_>>();
            assert_eq!(value.len(), 14, "unsupported build plan schema");
            let source = Path::new(value[2]);
            let source = source.strip_prefix("tests/compat").unwrap_or(source);
            let executable = source.with_extension("");
            let guest = Path::new("/").join(executable).to_string_lossy().into_owned();
            ((value[0].into(), value[1].into(), value[3].into()), guest)
        })
        .collect()
}

fn unsupported(setup: &Setup) -> Option<String> {
    if matches!(
        setup.fixture.as_str(),
        "executable"
            | "network-sandbox"
            | "multi-process-service"
            | "side-file"
            | "directory-tree"
            | "entry-symlink"
            | "special-device"
    ) || matches!(
        setup.fixture.as_str(),
        "rootfs-executable" | "rootfs-tree" | "rootfs-interpreter"
    ) && matches!(
        setup.rootfs.as_str(),
        "scratch-rootfs" | "mapping-data-rootfs" | "alpine-rootfs" | "dynamic-rootfs"
    ) {
        None
    } else {
        Some(format!("unsupported-fixture:{}", setup.fixture))
    }
}

fn setup(root: &Path) -> BTreeMap<(String, String, String), Setup> {
    let text = fs::read_to_string(root.join("fixture-schema.tsv")).unwrap();
    let setup = text
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let value = line.split('\t').collect::<Vec<_>>();
            assert_eq!(value.len(), 17, "unsupported fixture schema");
            let setup = Setup {
                fixture: value[4].into(),
                arguments: value[5].into(),
                side_files: value[10].into(),
                rootfs: value[11].into(),
            };
            ((value[0].into(), value[1].into(), value[2].into()), setup)
        })
        .collect::<BTreeMap<_, _>>();
    let inventory = fs::read_to_string(root.join("inventory.tsv")).unwrap();
    assert_eq!(
        setup.len(),
        inventory.lines().skip(1).filter(|line| !line.is_empty()).count(),
        "fixture schema drift",
    );
    setup
}

fn path(root: &Path, value: &str) -> Option<PathBuf> {
    (value != "-").then(|| root.join(value))
}

fn rootfs_argument(root: &Path, category: &str) -> String {
    if category == "dynamic-rootfs" {
        format!("dynamic-rootfs={}", root.join("artifacts/runtime").display())
    } else {
        category.to_owned()
    }
}

fn setting(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn worker_environment(row: &str, appended: Option<&str>) -> Result<String, &'static str> {
    let Some(appended) = appended else {
        return Ok(row.to_owned());
    };
    if appended == "-" || appended.is_empty() {
        return Err("HL_COMPAT_ENGINE_OPTIONS must contain semicolon-separated NAME=VALUE options");
    }
    for assignment in appended.split(';') {
        let Some((name, _)) = assignment.split_once('=') else {
            return Err("HL_COMPAT_ENGINE_OPTIONS assignment is missing '='");
        };
        if name.is_empty() || !hl_engine::options::Options::defines(name) {
            return Err("HL_COMPAT_ENGINE_OPTIONS contains an unknown engine option");
        }
    }
    Ok(if row == "-" {
        appended.to_owned()
    } else {
        format!("{row};{appended}")
    })
}

fn switch(name: &str) -> bool {
    match setting(name).as_deref() {
        None => false,
        Some("1") => true,
        Some(_) => panic!("{name} must be 1 when set"),
    }
}

fn positive(name: &str) -> Option<usize> {
    setting(name).map(|value| {
        value
            .parse::<usize>()
            .ok()
            .filter(|value| *value != 0)
            .unwrap_or_else(|| panic!("{name} must be a positive integer"))
    })
}

fn run_stamp(
    root: &Path,
    selection: &Selection,
    cases: &[Case],
    timeout_profile: TimeoutProfile,
) -> io::Result<String> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    for relative in ["inventory.tsv", "fixture-schema.tsv", "artifacts/manifest.tsv"] {
        stamp_file(&mut digest, relative, &root.join(relative))?;
    }
    for (label, path) in [
        ("inventory-runner", std::env::current_exe()?),
        ("compat-worker", PathBuf::from(env!("CARGO_BIN_EXE_hl-compat-worker"))),
        (
            "arm-engine",
            engine_binary::EngineBinaryPaths::required().named("hl-aarch64"),
        ),
        (
            "x86-engine",
            engine_binary::EngineBinaryPaths::required().named("hl-x86_64"),
        ),
        ("authority", PathBuf::from(env!("CARGO_BIN_EXE_hl-authority-child"))),
        ("projection", PathBuf::from(env!("CARGO_BIN_EXE_hl-projection-worker"))),
    ] {
        stamp_file(&mut digest, label, &path)?;
    }
    stamp_tree(&mut digest, &root.join("artifacts/runtime"))?;
    for case in cases {
        stamp_case(&mut digest, case)?;
    }
    stamp_value(
        &mut digest,
        "platform",
        format!(
            "{}\t{}\t{:?}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            Host::current()
        )
        .as_bytes(),
    );
    stamp_value(&mut digest, "selection", format!("{selection:?}").as_bytes());
    stamp_timeout_profile(&mut digest, timeout_profile);
    for setting in ["HL_COMPAT_TRACE", "HL_COMPAT_STALL_MS", "HL_COMPAT_JOBS"] {
        stamp_value(
            &mut digest,
            setting,
            std::env::var_os(setting)
                .as_ref()
                .map_or(b"-".as_slice(), |value| value.as_encoded_bytes()),
        );
    }
    stamp_engine_options(&mut digest, setting("HL_COMPAT_ENGINE_OPTIONS").as_deref());
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn stamp_case(digest: &mut ring::digest::Context, case: &Case) -> io::Result<()> {
    stamp_file(digest, "guest", &case.artifact)?;
    for (label, path) in [("stdout", case.stdout.as_ref()), ("stderr", case.stderr.as_ref())] {
        match path {
            Some(path) => stamp_file(digest, label, path)?,
            None => stamp_value(digest, label, b"-"),
        }
    }
    if case.side_files == "-" {
        return Ok(());
    }
    let source = case
        .artifact
        .ancestors()
        .nth(4)
        .ok_or_else(|| io::Error::other("compatibility side-file root missing"))?
        .join(&case.side_files);
    stamp_optional(digest, "side-file", &source)
}

fn stamp_optional(digest: &mut ring::digest::Context, label: &str, path: &Path) -> io::Result<()> {
    stamp_input(digest, label, path, false)
}

fn stamp_tree(digest: &mut ring::digest::Context, root: &Path) -> io::Result<()> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            collect_path(path, &mut pending, &mut paths);
        }
    }
    paths.sort();
    for path in paths {
        stamp_file(digest, "resource", &path)?;
    }
    Ok(())
}

fn collect_path(path: PathBuf, pending: &mut Vec<PathBuf>, paths: &mut Vec<PathBuf>) {
    if path.is_dir() {
        pending.push(path);
    } else {
        paths.push(path);
    }
}

fn stamp_file(digest: &mut ring::digest::Context, label: &str, path: &Path) -> io::Result<()> {
    stamp_input(digest, label, path, true)
}

fn stamp_input(digest: &mut ring::digest::Context, label: &str, path: &Path, required: bool) -> io::Result<()> {
    stamp_value(digest, "input-label", label.as_bytes());
    stamp_value(digest, "input-path", path.as_os_str().as_encoded_bytes());
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if !required && error.kind() == io::ErrorKind::NotFound => {
            stamp_value(digest, "input-state", b"absent");
            return Ok(());
        }
        Err(error) => return Err(fingerprint_error(path, error)),
    };
    stamp_value(digest, "input-state", b"present");
    let length = file.metadata().map_err(|error| fingerprint_error(path, error))?.len();
    digest.update(&length.to_be_bytes());
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| fingerprint_error(path, error))?;
        if count == 0 {
            return Ok(());
        }
        digest.update(&buffer[..count]);
    }
}

fn fingerprint_error(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("fingerprint {}: {error}", path.display()))
}

fn stamp_value(digest: &mut ring::digest::Context, label: &str, value: &[u8]) {
    digest.update(&(label.len() as u64).to_be_bytes());
    digest.update(label.as_bytes());
    digest.update(&(value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn stamp_engine_options(digest: &mut ring::digest::Context, value: Option<&str>) {
    stamp_value(digest, "HL_COMPAT_ENGINE_OPTIONS", value.map_or(b"-", str::as_bytes));
}

fn stamp_timeout_profile(digest: &mut ring::digest::Context, profile: TimeoutProfile) {
    stamp_value(digest, "HL_COMPAT_TIMEOUT_PROFILE", format!("{profile:?}").as_bytes());
}

fn stamped_timeout_profile(profile: TimeoutProfile) -> Vec<u8> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    stamp_timeout_profile(&mut digest, profile);
    digest.finish().as_ref().to_vec()
}

fn stamped_engine_options(value: Option<&str>) -> Vec<u8> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    stamp_engine_options(&mut digest, value);
    digest.finish().as_ref().to_vec()
}

fn stamped_input(path: &Path, required: bool) -> io::Result<Vec<u8>> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    stamp_input(&mut digest, "fixture", path, required)?;
    Ok(digest.finish().as_ref().to_vec())
}

const DEFAULT_JOBS: usize = 1;
fn corpus_path(root: Option<&str>) -> PathBuf {
    root.map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/runtime/legacy"), PathBuf::from)
}

fn corpus() -> PathBuf {
    corpus_path(setting("HL_COMPAT_ROOT").as_deref())
}

#[test]
fn corpus_override() {
    assert_eq!(
        corpus_path(Some("/persistent/corpus")),
        PathBuf::from("/persistent/corpus")
    );
}

#[test]
fn dynamic_rootfs_belongs_to_selected_corpus() {
    assert_eq!(
        rootfs_argument(Path::new("/corpus"), "dynamic-rootfs"),
        "dynamic-rootfs=/corpus/artifacts/runtime"
    );
    assert_eq!(
        rootfs_argument(Path::new("/corpus"), "scratch-rootfs"),
        "scratch-rootfs"
    );
}

#[test]
fn filtered_target() {
    let root = Path::new("/corpus");
    let canonical = ReportTarget::resolve(root, &Selection::default(), None);
    assert_eq!(canonical.0, root.join("report/api-results.tsv"));
    let filtered = Selection {
        suite: Some("completeness".into()),
        isa: Some("aarch64".into()),
        name: Some("abs/vector".into()),
    };
    let first = ReportTarget::resolve(root, &filtered, None);
    let second = ReportTarget::resolve(root, &filtered, None);
    assert_eq!(first.0, second.0);
    assert_ne!(first.0, canonical.0);
    assert_eq!(
        first.0.file_name().unwrap(),
        "api-results--suite-completeness--isa-aarch64--case-abs_2fvector.tsv"
    );
    let explicit = ReportTarget::resolve(root, &filtered, Some("chosen.tsv".into()));
    assert_eq!(explicit.0, PathBuf::from("chosen.tsv"));
}

#[test]
fn fixture_coverage() {
    let root = corpus();
    let fixtures = setup(&root);
    let inventory = fs::read_to_string(root.join("inventory.tsv")).unwrap();
    assert_eq!(
        fixtures.len(),
        inventory.lines().skip(1).filter(|line| !line.is_empty()).count(),
    );
}

#[test]
fn typed_fixtures_run() {
    let network = Setup {
        fixture: "network-sandbox".into(),
        arguments: "-".into(),
        side_files: "-".into(),
        rootfs: "-".into(),
    };
    let device = Setup {
        fixture: "special-device".into(),
        arguments: "-".into(),
        side_files: "-".into(),
        rootfs: "-".into(),
    };
    assert_eq!(unsupported(&network), None);
    assert_eq!(unsupported(&device), None);
}

#[test]
fn dispositions_follow_host() {
    for host in [Host::Linux, Host::Macos, Host::Windows] {
        assert!(host.accepts("active"));
        assert!(!host.accepts("excluded-known-bug"));
    }
    assert!(!Host::Macos.accepts("excluded-macos"));
    assert!(Host::Linux.accepts("excluded-macos"));
    assert!(Host::Windows.accepts("excluded-macos"));
    assert!(!Host::Windows.accepts("excluded-windows"));
    assert!(Host::Linux.accepts("excluded-windows"));
    assert!(Host::Macos.accepts("excluded-windows"));
}

#[test]
fn absent_stderr_golden_leaves_diagnostics_unconstrained() {
    assert_eq!(
        mismatch(true, true, true, 0, "0", b"ok\n", b"ok\n", None, b"diagnostic\n"),
        "-"
    );
    assert!(
        mismatch(
            true,
            true,
            true,
            0,
            "0",
            b"ok\n",
            b"ok\n",
            Some(b"expected\n"),
            b"diagnostic\n",
        )
        .starts_with("stderr:")
    );
}

#[test]
fn process_fixture_runs() {
    let setup = Setup {
        fixture: "multi-process-service".into(),
        arguments: "-".into(),
        side_files: "-".into(),
        rootfs: "-".into(),
    };
    assert_eq!(unsupported(&setup), None);
}

#[test]
fn wait_reaps() {
    let output = std::env::temp_dir().join(format!("hl-wait-output-{}", std::process::id()));
    let error = std::env::temp_dir().join(format!("hl-wait-error-{}", std::process::id()));
    fs::write(&output, []).unwrap();
    fs::write(&error, []).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hl-native-child-fixture"));
    command.env("HL_FIXTURE_BLOCK", "1");
    supervision::configure(&mut command);
    let mut child = command.spawn().unwrap();
    assert!(matches!(
        supervision::wait(&mut child, Duration::from_millis(10), None, &output, &error).unwrap(),
        supervision::Outcome::TimedOut(_),
    ));
    assert!(child.try_wait().unwrap().is_some());
    fs::remove_file(output).unwrap();
    fs::remove_file(error).unwrap();
}

#[test]
#[ignore = "bounded native execution diagnostic"]
fn native_recursion_counters() {
    #[cfg(unix)]
    let _signals =
        hl_engine::native::TerminationSignals::install().expect("install compatibility termination handlers");
    let root = corpus();
    let case = parse(&root, TimeoutProfile::Compatibility)
        .into_iter()
        .find(|case| case.suite == "abi" && case.name == "recursion" && case.isa == "aarch64")
        .expect("aarch64 recursion inventory row");
    let scratch = std::env::temp_dir().join(format!(
        "hl-native-counters-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&scratch).unwrap();
    let report = scratch.join("result");
    let output_path = scratch.join("stdout");
    let error_path = scratch.join("stderr");
    let output = fs::File::create(&output_path).unwrap();
    let error = fs::File::create(&error_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hl-compat-worker"));
    supervision::configure(&mut command);
    command
        .arg(&case.isa)
        .arg(&case.artifact)
        .arg("HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1")
        .arg(&report)
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(error))
        .arg(&case.fixture)
        .arg(&case.arguments)
        .arg(&case.side_files)
        .arg(&case.rootfs);
    let mut child = command.spawn().unwrap();
    let wall = Duration::from_millis(case.timeout);
    let outcome = supervision::wait(
        &mut child,
        wall,
        supervision::stall_budget(wall),
        &output_path,
        &error_path,
    )
    .unwrap();
    if !matches!(&outcome, supervision::Outcome::Exited(status) if status.success()) {
        supervision::contain(&mut child).unwrap();
    }
    let stderr = fs::read_to_string(&error_path).unwrap();
    let result = fs::read_to_string(&report).unwrap_or_default();
    fs::remove_dir_all(&scratch).unwrap();
    assert!(matches!(outcome, supervision::Outcome::Exited(status) if status.success()));
    assert!(result.starts_with("0\ntrue\nEngineExit { kind: Code, guest_status: 0"));
    let counters = stderr
        .lines()
        .find_map(|line| line.strip_prefix("hl-native: "))
        .expect("native diagnostic counters");
    println!("hl-native: {counters}");
    let values = counters
        .split_whitespace()
        .map(|field| field.split_once('=').expect("named native counter"))
        .collect::<BTreeMap<_, _>>();
    let fallbacks = values["fallbacks"].parse::<u64>().unwrap();
    let sites = values["sites"].parse::<u64>().unwrap();
    assert_eq!(fallbacks, 0, "native fallback regression");
    assert_eq!(sites, 0, "native fallback-site regression");
}

#[test]
fn jobs_default_serial() {
    assert_eq!(DEFAULT_JOBS, 1);
}

#[test]
fn compatibility_deadlines() {
    assert_eq!(TimeoutProfile::Compatibility.deadline(120_000), 120_000);
    assert_eq!(TimeoutProfile::Compatibility.deadline(240_000), 240_000);
}

#[test]
fn performance_deadlines() {
    assert_eq!(TimeoutProfile::Performance.deadline(120_000), 10_000);
    assert_eq!(TimeoutProfile::Performance.deadline(240_000), 10_000);
    assert_eq!(TimeoutProfile::Performance.deadline(3_000), 3_000);
}

#[test]
fn profile_fingerprint() {
    assert_ne!(
        stamped_timeout_profile(TimeoutProfile::Compatibility),
        stamped_timeout_profile(TimeoutProfile::Performance),
    );
}

#[test]
fn absent_engine_options_preserve_worker_environment() {
    for row in ["-", "HL_CPUS=2", "A=B;HL_CPUS=2"] {
        assert_eq!(worker_environment(row, None).unwrap(), row);
    }
}

#[test]
fn harness_engine_options_append_and_take_precedence() {
    assert_eq!(
        worker_environment("HL_NATIVE_EXECUTION=0;GUEST=value", Some("HL_NATIVE_EXECUTION=1")),
        Ok("HL_NATIVE_EXECUTION=0;GUEST=value;HL_NATIVE_EXECUTION=1".into()),
    );
    let mut options = hl_engine::options::Options::default();
    for assignment in worker_environment("HL_NATIVE_EXECUTION=0", Some("HL_NATIVE_EXECUTION=1"))
        .unwrap()
        .split(';')
    {
        let (name, value) = assignment.split_once('=').unwrap();
        options.set(name, value, true).unwrap();
    }
    assert_eq!(options.get("HL_NATIVE_EXECUTION"), Some("1"));
}

#[test]
fn harness_engine_options_reject_non_options() {
    assert!(worker_environment("-", Some("PATH=/tmp")).is_err());
    assert!(worker_environment("-", Some("HL_NATIVE_EXECUTION")).is_err());
    assert!(worker_environment("-", Some("-")).is_err());
}

#[test]
fn engine_options_are_resume_fingerprinted() {
    let absent = stamped_engine_options(None);
    let native = stamped_engine_options(Some("HL_NATIVE_EXECUTION=1"));
    let diagnostics = stamped_engine_options(Some("HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1"));
    assert_ne!(absent, native);
    assert_ne!(native, diagnostics);
}

#[test]
fn optional_absence_stamped() {
    let root = fingerprint_scratch("optional");
    fs::create_dir_all(&root).unwrap();
    let first = stamped_input(&root.join("first"), false).unwrap();
    let second = stamped_input(&root.join("second"), false).unwrap();
    assert_ne!(first, second, "declared input path must be part of the stamp");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn required_absence_fails() {
    let root = fingerprint_scratch("required");
    fs::create_dir_all(&root).unwrap();
    let missing = root.join("missing");
    let error = stamped_input(&missing, true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(error.to_string().contains(&missing.display().to_string()));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn present_bytes_rehash() {
    let root = fingerprint_scratch("present");
    fs::create_dir_all(&root).unwrap();
    let input = root.join("input");
    fs::write(&input, b"alpha").unwrap();
    let first = stamped_input(&input, false).unwrap();
    fs::write(&input, b"bravo").unwrap();
    let second = stamped_input(&input, false).unwrap();
    assert_ne!(first, second);
    fs::remove_dir_all(root).unwrap();
}

fn fingerprint_scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "hl-fingerprint-{name}-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed),
    ))
}

#[cfg(target_os = "linux")]
#[test]
fn descendants_reaped() {
    let root = std::env::temp_dir().join(format!(
        "hl-descendant-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir_all(&root).unwrap();
    let identity = root.join("pid");
    let output = root.join("stdout");
    let error = root.join("stderr");
    fs::write(&output, []).unwrap();
    fs::write(&error, []).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_hl-native-child-fixture"));
    command
        .env("HL_FIXTURE_BLOCK", "1")
        .env("HL_FIXTURE_ESCAPE", "1")
        .env("HL_FIXTURE_DESCENDANT", &identity);
    supervision::configure(&mut command);
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !identity.exists() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let descendant = fs::read_to_string(&identity).unwrap();
    assert!(matches!(
        supervision::wait(&mut child, Duration::from_millis(10), None, &output, &error).unwrap(),
        supervision::Outcome::TimedOut(_),
    ));
    let process = PathBuf::from(format!("/proc/{}", descendant.trim()));
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while process.exists() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!process.exists(), "compatibility descendant survived teardown");
    fs::remove_dir_all(root).unwrap();
}
