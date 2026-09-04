use crate::suite::Error;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Isa {
    Aarch64,
    X86_64,
}

impl Isa {
    const fn name(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Backend {
    Native,
    Translated,
}

#[derive(Args)]
pub(crate) struct Options {
    /// Full commit identifying the source checkout supplying the campaign.
    #[arg(long, value_parser = full_sha)]
    source_sha: String,
    /// Exact production architecture worker to execute.
    #[arg(long)]
    engine: PathBuf,
    /// Exact native library selected by the worker.
    #[arg(long)]
    native_library: PathBuf,
    /// Root filesystem containing the immutable probe and its control directory.
    #[arg(long)]
    rootfs: PathBuf,
    /// Guest-relative executable path inside the root filesystem.
    #[arg(long)]
    probe: PathBuf,
    /// Absolute guest path to a new, empty probe control directory.
    #[arg(long)]
    control: PathBuf,
    #[arg(long, value_enum)]
    guest_isa: Isa,
    #[arg(long, value_enum)]
    backend: Backend,
    /// Optional cross-host emulator; evidence produced through it is semantic-only.
    #[arg(long)]
    emulator: Option<PathBuf>,
    #[arg(long, requires = "emulator")]
    emulator_arg: Vec<String>,
    /// New directory receiving the immutable identity and captured streams.
    #[arg(long)]
    results: PathBuf,
    /// Hard deadline for each engine or emulator process.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=900))]
    timeout_seconds: u64,
}

fn full_sha(value: &str) -> Result<String, String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(value.to_owned())
    } else {
        Err("source SHA must be 40 lowercase hexadecimal characters".into())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendReceipt {
    schema: String,
    backend: String,
    engine_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoaderReceipt {
    schema: String,
    library_sha256: String,
    library_path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CycleReceipt {
    schema: String,
    guest_isa: String,
    backend: String,
    captured: bool,
    original_reaped: bool,
    restored_killed: bool,
    kill_exit_kind: String,
    kill_guest_status: i32,
    restored_continued: bool,
    member_count: usize,
    image_sha256: String,
    transcript_sha256: String,
    final_guest_status: i32,
}

#[derive(Serialize)]
struct Identity<'a> {
    schema: &'static str,
    source_sha: &'a str,
    host_isa: &'static str,
    guest_isa: &'static str,
    backend: Backend,
    evidence_class: &'static str,
    engine_sha256: String,
    native_library_sha256: String,
    rootfs_sha256: String,
    probe_sha256: String,
    emulator_sha256: Option<String>,
    emulator_argv_sha256: Option<String>,
    stdout_sha256: String,
    stderr_sha256: String,
    checkpoint_image_sha256: String,
    checkpoint_member_count: usize,
    transcript_sha256: String,
}

const fn host_isa() -> Option<Isa> {
    #[cfg(target_arch = "aarch64")]
    return Some(Isa::Aarch64);
    #[cfg(target_arch = "x86_64")]
    return Some(Isa::X86_64);
    #[allow(unreachable_code)]
    None
}

fn record<T: for<'de> Deserialize<'de>>(stderr: &str, prefix: &str) -> Result<T, Error> {
    let records = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(format!("expected exactly one {prefix:?} record, got {}", records.len()).into());
    }
    Ok(serde_json::from_str(records[0])?)
}

fn hash_file(path: &Path) -> Result<String, Error> {
    let bytes = std::fs::read(path)?;
    Ok(hex_bytes(&Sha256::digest(&bytes)))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn argv_hash(arguments: &[String]) -> String {
    let mut digest = Sha256::new();
    for argument in arguments {
        digest.update((argument.len() as u64).to_be_bytes());
        digest.update(argument.as_bytes());
    }
    hex_bytes(&digest.finalize())
}

fn output_until(
    program: &Path,
    prefix: &[String],
    arguments: &[String],
    timeout: Duration,
) -> Result<std::process::Output, Error> {
    fn kill_and_reap(child: &mut std::process::Child) {
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .status();
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let mut command = Command::new(program);
    command.args(prefix).args(arguments);
    command.stdout(std::process::Stdio::from(stdout.reopen()?));
    command.stderr(std::process::Stdio::from(stderr.reopen()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(format!(
                    "cannot observe {} before its deadline; process group was killed and reaped: {error}",
                    program.display()
                )
                .into());
            }
        }
        if Instant::now() >= deadline {
            kill_and_reap(&mut child);
            return Err(format!(
                "{} exceeded its {timeout:?} deadline and was killed and reaped",
                program.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(std::process::Output {
        status,
        stdout: std::fs::read(stdout.path())?,
        stderr: std::fs::read(stderr.path())?,
    })
}

fn validate_cycle(receipt: &CycleReceipt, options: &Options) -> Result<(), Error> {
    let expected_backend = match options.backend {
        Backend::Native => "native",
        Backend::Translated => "translated",
    };
    if receipt.schema != "husklet-checkpoint-cycle-v1"
        || receipt.guest_isa != options.guest_isa.name()
        || receipt.backend != expected_backend
        || !receipt.captured
        || !receipt.original_reaped
        || !receipt.restored_killed
        || receipt.kill_exit_kind != "Signal"
        || receipt.kill_guest_status != 9
        || !receipt.restored_continued
        || receipt.member_count == 0
        || !lower_hex(&receipt.image_sha256)
        || !lower_hex(&receipt.transcript_sha256)
        || receipt.final_guest_status != 0
    {
        return Err(format!("checkpoint worker emitted an invalid lifecycle receipt: {receipt:?}").into());
    }
    Ok(())
}

fn evidence_class(host: Isa, guest: Isa, backend: Backend, emulated: bool) -> Result<&'static str, Error> {
    if backend == Backend::Native && host != guest && !emulated {
        return Err("native checkpoint evidence requires identical host and guest ISAs".into());
    }
    Ok(if emulated {
        "semantic_emulated"
    } else {
        "semantic_physical"
    })
}

fn lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn guest_path(rootfs: &Path, guest: &Path) -> Result<PathBuf, Error> {
    if !guest.is_absolute()
        || guest
            .components()
            .skip(1)
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("guest control path must be absolute and normalized".into());
    }
    let root = std::fs::canonicalize(rootfs)?;
    let host = std::fs::canonicalize(rootfs.join(guest.strip_prefix("/")?))?;
    if !host.starts_with(root) {
        return Err("guest control path escapes the rootfs".into());
    }
    Ok(host)
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    let host = host_isa().ok_or("checkpoint-backend supports only aarch64 and x86_64 hosts")?;
    let evidence_class = evidence_class(host, options.guest_isa, options.backend, options.emulator.is_some())?;
    if options.results.exists() {
        return Err(format!("results directory {} already exists", options.results.display()).into());
    }
    let workspace = crate::runtime::workspace()?;
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace)
        .output()?;
    if !head.status.success() || String::from_utf8(head.stdout)?.trim() != options.source_sha {
        return Err("--source-sha does not match the repository HEAD".into());
    }
    let engine_sha256 = hash_file(&options.engine)?;
    let native_library_sha256 = hash_file(&options.native_library)?;
    let rootfs_sha256 = crate::benchmark::artifact_identity(&options.rootfs)?;
    if options.probe.is_absolute()
        || options
            .probe
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("--probe must be a normalized path relative to the rootfs".into());
    }
    let probe_host = options.rootfs.join(&options.probe);
    let probe_sha256 = hash_file(&probe_host)?;

    let timeout = Duration::from_secs(options.timeout_seconds);
    let smoke = if let Some(emulator) = &options.emulator {
        let mut arguments = options.emulator_arg.clone();
        arguments.push(options.engine.display().to_string());
        output_until(emulator, &arguments, &["--backend-receipt".into()], timeout)?
    } else {
        output_until(&options.engine, &[], &["--backend-receipt".into()], timeout)?
    };
    if !smoke.status.success() {
        return Err("selected engine refused its backend receipt".into());
    }
    let backend_receipt: BackendReceipt = serde_json::from_slice(&smoke.stdout)?;
    if backend_receipt.schema != "husklet-engine-backend-v1"
        || backend_receipt.backend != "retained-c"
        || backend_receipt.engine_sha256 != engine_sha256
    {
        return Err("selected engine backend receipt does not match its artifact".into());
    }

    let mut worker_arguments = vec![
        "--guest-isa".to_owned(),
        options.guest_isa.name().to_owned(),
        "--native-library".to_owned(),
        options.native_library.display().to_string(),
        "--loader-receipt".to_owned(),
        "--rootfs".to_owned(),
        options.rootfs.display().to_string(),
        "--checkpoint-cycle".to_owned(),
        options.control.display().to_string(),
    ];
    if options.backend == Backend::Translated {
        worker_arguments.extend(["--translit".to_owned(), "--diagnostics".to_owned()]);
    } else {
        worker_arguments.extend(["--native-supervised=on".to_owned()]);
    }
    worker_arguments.push(options.probe.display().to_string());

    let (program, prefix, emulator_sha256, emulator_argv_sha256) = if let Some(emulator) = &options.emulator {
        let mut prefix = options.emulator_arg.clone();
        prefix.push(options.engine.display().to_string());
        let emulator_sha256 = hash_file(emulator)?;
        let emulator_argv_sha256 = argv_hash(&prefix);
        (emulator, prefix, Some(emulator_sha256), Some(emulator_argv_sha256))
    } else {
        (&options.engine, Vec::new(), None, None)
    };
    let output = output_until(program, &prefix, &worker_arguments, timeout)?;
    if !output.status.success() {
        return Err(format!("checkpoint worker failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    let stderr = std::str::from_utf8(&output.stderr)?;
    let loader: LoaderReceipt = record(stderr, "[hl-loader]\t")?;
    if loader.schema != "husklet-engine-loader-v1"
        || loader.library_sha256 != native_library_sha256
        || std::fs::canonicalize(loader.library_path)? != std::fs::canonicalize(&options.native_library)?
    {
        return Err("worker loader receipt does not match the selected native library".into());
    }
    let cycle: CycleReceipt = record(stderr, "[hl-checkpoint-cycle]\t")?;
    validate_cycle(&cycle, &options)?;
    if options.backend == Backend::Translated {
        crate::runtime::validate_translated_execution(&output.stderr)?;
    }
    let control_host = guest_path(&options.rootfs, &options.control)?;
    let image_artifact = std::fs::read(control_host.join("checkpoint-image.bin"))?;
    let transcript_artifact = std::fs::read(control_host.join("checkpoint-transcript.bin"))?;
    if hex_bytes(&Sha256::digest(&image_artifact)) != cycle.image_sha256
        || hex_bytes(&Sha256::digest(&transcript_artifact)) != cycle.transcript_sha256
    {
        return Err("exported checkpoint artifacts do not match the independently recomputed receipt hashes".into());
    }
    let identity = Identity {
        schema: "husklet-checkpoint-backend-v1",
        source_sha: &options.source_sha,
        host_isa: host.name(),
        guest_isa: options.guest_isa.name(),
        backend: options.backend,
        evidence_class,
        engine_sha256,
        native_library_sha256,
        rootfs_sha256,
        probe_sha256,
        emulator_sha256,
        emulator_argv_sha256,
        stdout_sha256: hex_bytes(&Sha256::digest(&output.stdout)),
        stderr_sha256: hex_bytes(&Sha256::digest(&output.stderr)),
        checkpoint_image_sha256: cycle.image_sha256,
        checkpoint_member_count: cycle.member_count,
        transcript_sha256: cycle.transcript_sha256,
    };
    let parent = options.results.parent().unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".checkpoint-backend-")
        .tempdir_in(parent)?;
    for (name, bytes) in [
        ("stdout.bin", output.stdout),
        ("stderr.bin", output.stderr),
        ("checkpoint-image.bin", image_artifact),
        ("checkpoint-transcript.bin", transcript_artifact),
        ("identity.json", serde_json::to_vec_pretty(&identity)?),
    ] {
        let path = staging.path().join(name);
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::File::open(staging.path())?.sync_all()?;
    let staged_path = staging.keep();
    if let Err(error) = std::fs::rename(&staged_path, &options.results) {
        return Err(format!(
            "cannot publish checkpoint results {} as {}: {error}",
            staged_path.display(),
            options.results.display()
        )
        .into());
    }
    std::fs::File::open(parent)?.sync_all()?;
    println!("CHECKPOINT_BACKEND {}", options.results.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_hex_is_exact_lowercase_and_known() {
        for (input, expected) in [
            (
                b"".as_slice(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".as_slice(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            let actual = hex_bytes(&Sha256::digest(input));
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), 64);
            assert!(
                actual
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
        }
    }

    #[test]
    fn source_identity_is_full_lowercase_sha() {
        assert!(full_sha(&"a".repeat(40)).is_ok());
        for invalid in [
            "a",
            "A0000000000000000000000000000000000000000",
            "z0000000000000000000000000000000000000000",
        ] {
            assert!(full_sha(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn lifecycle_receipt_rejects_missing_semantic_legs() {
        let json = r#"{"schema":"husklet-checkpoint-cycle-v1","guest_isa":"x86_64","backend":"translated","captured":true,"original_reaped":true,"restored_killed":false,"kill_exit_kind":"Signal","kill_guest_status":9,"restored_continued":true,"member_count":2,"image_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","transcript_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","final_guest_status":0}"#;
        let receipt: CycleReceipt = serde_json::from_str(json).unwrap();
        let options = Options {
            source_sha: "a".repeat(40),
            engine: "engine".into(),
            native_library: "lib".into(),
            rootfs: "root".into(),
            probe: "bin/probe".into(),
            control: "/run/cycle".into(),
            guest_isa: Isa::X86_64,
            backend: Backend::Translated,
            emulator: None,
            emulator_arg: Vec::new(),
            results: "results".into(),
            timeout_seconds: 120,
        };
        assert!(validate_cycle(&receipt, &options).is_err());
        let code_exit = json
            .replace("\"restored_killed\":false", "\"restored_killed\":true")
            .replace("\"kill_exit_kind\":\"Signal\"", "\"kill_exit_kind\":\"Code\"");
        let receipt: CycleReceipt = serde_json::from_str(&code_exit).unwrap();
        assert!(validate_cycle(&receipt, &options).is_err());
    }

    #[test]
    fn strict_receipts_reject_unknown_fields_and_duplicates() {
        let stderr = "[hl-loader]\t{\"schema\":\"x\",\"library_sha256\":\"x\",\"library_path\":\"x\",\"extra\":1}\n";
        assert!(record::<LoaderReceipt>(stderr, "[hl-loader]\t").is_err());
        let duplicate = format!("{stderr}{stderr}");
        assert!(record::<LoaderReceipt>(&duplicate, "[hl-loader]\t").is_err());
    }

    #[test]
    fn selection_matrix_represents_four_directions_without_timing_claims() {
        for host in [Isa::Aarch64, Isa::X86_64] {
            for guest in [Isa::Aarch64, Isa::X86_64] {
                assert!(evidence_class(host, guest, Backend::Translated, false).is_ok());
                assert_eq!(
                    evidence_class(host, guest, Backend::Native, false).is_ok(),
                    host == guest
                );
                assert_eq!(
                    evidence_class(host, guest, Backend::Native, true).unwrap(),
                    "semantic_emulated"
                );
                assert_eq!(
                    evidence_class(host, guest, Backend::Translated, true).unwrap(),
                    "semantic_emulated"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn child_deadline_kills_and_reaps_the_process_group() {
        let started = Instant::now();
        let error = output_until(
            Path::new("/bin/sh"),
            &[],
            &["-c".into(), "sleep 30".into()],
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.to_string().contains("was killed and reaped"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
