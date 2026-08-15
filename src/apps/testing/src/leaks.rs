use clap::Args;
use std::{
    error::Error,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_CASES: &[(&str, &str)] = &[
    ("runtime/job-control/lifecycle", "arm64"),
    ("runtime/process/forkwait", "arm64"),
    ("runtime/process/exec-self", "arm64"),
    ("runtime/libc/malloc-big", "arm64"),
    ("runtime/workload/sqlite", "arm64"),
    ("runtime/process/forkwait", "amd64"),
];

#[derive(Args)]
pub(crate) struct Options {
    /// New relative artifact directory beneath the repository workspace.
    #[arg(long, default_value = "target/testing/leaks/latest")]
    artifacts: PathBuf,
    /// Hard limit for each workload and for the deliberate-leak probe.
    #[arg(long, default_value_t = 180)]
    timeout_seconds: u64,
    /// `LSan` suppression file. Every suppression must be reviewed and checked in.
    #[arg(long, default_value = "tests/lsan.supp")]
    suppressions: PathBuf,
}

struct Outcome {
    status: ExitStatus,
    timed_out: bool,
}

pub(crate) fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let workspace = crate::runtime::workspace()?;
    let artifacts = checked_artifacts(&workspace, &options.artifacts)?;
    fs::create_dir_all(&artifacts)?;
    let suppression = workspace.join(&options.suppressions);
    if !suppression.is_file() {
        return Err(format!("leak suppression file is absent: {}", suppression.display()).into());
    }
    let executable = std::env::current_exe()?;
    let mut report = File::create(artifacts.join("results.tsv"))?;
    writeln!(report, "case\tisa\tstatus\tdiagnostic")?;
    let timeout = Duration::from_secs(options.timeout_seconds);
    let mut failed = false;

    let label = "non-vacuity";
    let stdout = File::create(artifacts.join("non-vacuity.stdout"))?;
    let stderr = File::create(artifacts.join("non-vacuity.stderr"))?;
    let log = artifacts.join("non-vacuity.sanitizer");
    let mut probe = crate::platform::HostProcess::standard(&executable);
    probe
        .arg("leak-probe")
        .env(
            "ASAN_OPTIONS",
            format!("detect_leaks=1:halt_on_error=1:exitcode=97:log_path={}", log.display()),
        )
        .env(
            "LSAN_OPTIONS",
            format!(
                "suppressions={}:print_suppressions=1:exitcode=97:log_path={}",
                suppression.display(),
                log.display()
            ),
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = probe.spawn()?;
    let probe_outcome = wait(&mut child, timeout)?;
    let probe_sanitizer = sanitizer_reported(&artifacts, label)?;
    if !non_vacuity_passed(probe_outcome.status.code(), probe_outcome.timed_out, probe_sanitizer) {
        let probe_diagnostic = diagnostic(&probe_outcome, probe_sanitizer);
        writeln!(report, "non-vacuity\tarm64\tfail\t{probe_diagnostic}")?;
        report.flush()?;
        eprintln!("leaks: artifacts={}", artifacts.display());
        return Err(non_vacuity_failure(&probe_diagnostic).into());
    }

    for (index, &(case, isa)) in DEFAULT_CASES.iter().enumerate() {
        if case == "runtime/workload/sqlite" && !sqlite_available() {
            writeln!(
                report,
                "{case}\t{isa}\tskip\tcross sqlite development files unavailable"
            )?;
            continue;
        }
        let label = format!("{index:02}-{}-{isa}", case.replace('/', "-"));
        let outcome = run_case(
            &executable,
            &workspace,
            &artifacts,
            &suppression,
            case,
            isa,
            &label,
            timeout,
            false,
        )?;
        let sanitizer = sanitizer_reported(&artifacts, &label)?;
        let passed = outcome.status.success() && !outcome.timed_out && !sanitizer;
        writeln!(
            report,
            "{case}\t{isa}\t{}\t{}",
            if passed { "pass" } else { "fail" },
            diagnostic(&outcome, sanitizer)
        )?;
        failed |= !passed;
    }

    writeln!(
        report,
        "non-vacuity\tarm64\tpass\t{}",
        diagnostic(&probe_outcome, probe_sanitizer)
    )?;
    report.flush()?;
    eprintln!("leaks: artifacts={}", artifacts.display());
    if failed {
        Err("production C engine leak gate failed; inspect retained artifacts".into())
    } else {
        Ok(())
    }
}

fn non_vacuity_passed(exit_code: Option<i32>, timed_out: bool, sanitizer: bool) -> bool {
    !timed_out && exit_code == Some(97) && sanitizer
}

fn non_vacuity_failure(diagnostic: &str) -> String {
    format!(
        "LeakSanitizer non-vacuity probe failed before workloads ({diagnostic}); rebuild and run an instrumented binary in a dedicated target: HL_C_SANITIZER=leak CARGO_TARGET_DIR=target/lsan cargo run --locked --offline -p testing -- leaks --artifacts target/testing/leaks/lsan-UNIQUE"
    )
}

fn sqlite_available() -> bool {
    crate::platform::HostProcess::standard("aarch64-linux-gnu-gcc")
        .args(["-E", "-include", "sqlite3.h", "-x", "c", "/dev/null"])
        .env_remove("LD_PRELOAD")
        .env("ASAN_OPTIONS", "detect_leaks=0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn checked_artifacts(workspace: &Path, relative: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("leak artifact directory must be a relative path without '..'".into());
    }
    let path = workspace.join(relative);
    if path.exists() {
        return Err(format!("leak artifact directory already exists: {}", path.display()).into());
    }
    Ok(path)
}

#[allow(clippy::too_many_arguments)]
fn run_case(
    executable: &Path,
    workspace: &Path,
    artifacts: &Path,
    suppression: &Path,
    case: &str,
    isa: &str,
    label: &str,
    timeout: Duration,
    probe: bool,
) -> Result<Outcome, Box<dyn Error>> {
    let stdout = File::create(artifacts.join(format!("{label}.stdout")))?;
    let stderr = File::create(artifacts.join(format!("{label}.stderr")))?;
    let ledger = artifacts.join(format!("{label}.tsv"));
    let log = artifacts.join(format!("{label}.sanitizer"));
    let mut command = if cfg!(target_os = "macos") {
        let mut command = crate::platform::HostProcess::standard("leaks");
        command.args(["--atExit", "--"]);
        command.arg(executable);
        command
    } else {
        crate::platform::HostProcess::standard(executable)
    };
    command
        .current_dir(workspace)
        .args([
            "runtime",
            "--case",
            case,
            "--isa",
            isa,
            "--jobs",
            "1",
            "--engine-profile",
            "debug",
            "--results",
        ])
        .arg(ledger.strip_prefix(workspace)?)
        .env(
            "ASAN_OPTIONS",
            format!("detect_leaks=1:halt_on_error=1:exitcode=97:log_path={}", log.display()),
        )
        .env(
            "LSAN_OPTIONS",
            format!(
                "suppressions={}:print_suppressions=1:exitcode=97:log_path={}",
                suppression.display(),
                log.display()
            ),
        )
        .env(
            "HL_TEST_ENGINE_APP_BIN_DIR",
            executable.parent().ok_or("testing executable has no parent")?,
        )
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if probe {
        command.env("HL_LEAK_CHECK_PROBE", "1");
    }
    let mut child = command.spawn()?;
    wait(&mut child, timeout).map_err(Into::into)
}

fn wait(child: &mut std::process::Child, timeout: Duration) -> Result<Outcome, std::io::Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Outcome {
                status,
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            child.kill()?;
            return Ok(Outcome {
                status: child.wait()?,
                timed_out: true,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn sanitizer_reported(artifacts: &Path, label: &str) -> Result<bool, Box<dyn Error>> {
    for entry in fs::read_dir(artifacts)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(&format!("{label}.sanitizer")) {
            let metadata = fs::metadata(&path)?;
            if metadata.len() > 8 * 1024 * 1024 {
                return Err(format!("sanitizer output exceeds 8 MiB: {}", path.display()).into());
            }
            let text = fs::read_to_string(&path)?;
            if text.contains("LeakSanitizer") || text.contains("AddressSanitizer") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn diagnostic(outcome: &Outcome, sanitizer: bool) -> String {
    format!(
        "exit={} timeout={} sanitizer={sanitizer}",
        outcome.status, outcome.timed_out
    )
}

#[cfg(test)]
mod tests {
    use super::{non_vacuity_failure, non_vacuity_passed};

    #[test]
    fn non_vacuity_requires_exit_97_and_sanitizer_report() {
        assert!(non_vacuity_passed(Some(97), false, true));
        assert!(!non_vacuity_passed(Some(0), false, false));
        assert!(!non_vacuity_passed(Some(97), false, false));
        assert!(!non_vacuity_passed(Some(97), true, true));
    }

    #[test]
    fn non_vacuity_failure_explains_dedicated_instrumented_build() {
        let message = non_vacuity_failure("exit=exit status: 0 timeout=false sanitizer=false");
        assert!(message.contains("failed before workloads"));
        assert!(message.contains("HL_C_SANITIZER=leak"));
        assert!(message.contains("CARGO_TARGET_DIR=target/lsan"));
        assert!(message.contains("cargo run --locked --offline -p testing -- leaks"));
    }
}
