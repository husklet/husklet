//! Bounded CPU-profile acquisition and deterministic offline attribution.

use clap::{Args, Subcommand, ValueEnum};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type Error = Box<dyn std::error::Error>;
const PROFILE_COUNT: usize = 6;
const FORMAT: &str = "husklet-profile-attribution-v1";
const PERF_RING_BUFFER: &str = "32M";
const MAX_LOST_SAMPLE_PERCENT: f64 = 1.0;
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Args)]
pub(crate) struct Options {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Freeze artifacts and record exactly six resumable profiles.
    Record(RecordOptions),
    /// Attribute six previously recorded profiles without executing the workload.
    Parse(ParseOptions),
}

#[derive(Args)]
struct RecordOptions {
    #[arg(long)]
    results: PathBuf,
    #[arg(long)]
    executable: PathBuf,
    #[arg(long)]
    native_library: PathBuf,
    #[arg(long, value_enum)]
    execution_mode: ExecutionMode,
    /// Host executable used for the native before/after controls (normally chroot).
    #[arg(long)]
    native_executable: PathBuf,
    /// One native-control argument; repeat to preserve exact argv boundaries.
    #[arg(long = "native-arg", allow_hyphen_values = true, required = true)]
    native_command: Vec<String>,
    /// SHA-256 of the workload's exact stdout bytes.
    #[arg(long)]
    semantic_sha256: String,
    /// Workload artifact whose exact bytes carry semantics (for example, GCC's object file).
    #[arg(long, requires = "semantic_output_sha256")]
    semantic_output: Option<PathBuf>,
    #[arg(long, requires = "semantic_output")]
    semantic_output_sha256: Option<String>,
    /// Workload argv; its executable must be the frozen executable path.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum ExecutionMode {
    Interpreter,
    Translit,
}

#[derive(Args)]
struct ParseOptions {
    #[arg(long)]
    results: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Frozen {
    role: String,
    original: PathBuf,
    frozen: PathBuf,
    sha256: String,
    build_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Manifest {
    format: String,
    profiles: usize,
    event: String,
    frequency_hz: u32,
    ring_buffer: String,
    call_graph: String,
    direct_jmp: String,
    execution_mode: ExecutionMode,
    semantic_sha256: String,
    semantic_output: Option<PathBuf>,
    semantic_output_sha256: Option<String>,
    command: Vec<String>,
    native_command: Vec<String>,
    artifacts: Vec<Frozen>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AnchorReceipt {
    position: String,
    executable_sha256: String,
    stdout_sha256: String,
    stderr_sha256: String,
    semantic_output_sha256: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ModeProof {
    execution_mode: ExecutionMode,
    stderr_sha256: String,
    stdout_sha256: String,
    semantic_output_sha256: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct FinalSeal {
    format: String,
    manifest_sha256: String,
    ledger_sha256: String,
    proof_sha256: String,
    anchor_before_sha256: String,
    anchor_after_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Receipt {
    slot: usize,
    perf_data_sha256: String,
    script_sha256: String,
    exporter_stderr_sha256: String,
    stdout_sha256: String,
    semantic_output_sha256: Option<String>,
    stderr_sha256: String,
    symbol_index_sha256: String,
    direct_jmp_ibtc_disabled: bool,
    samples: u64,
    lost_records: u64,
    lost_chunks: u64,
    lost_sample_percent: f64,
}

#[derive(Debug, Default, PartialEq)]
struct ExportLoss {
    lost_records: u64,
    lost_chunks: u64,
    lost_sample_percent: f64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct Counts {
    total: u64,
    exact_elf: u64,
    memfd_jit: u64,
    invalid_unwind: u64,
    unresolved: u64,
}

impl Counts {
    fn verify(self) -> Result<Self, Error> {
        if self.total != self.exact_elf + self.memfd_jit + self.invalid_unwind + self.unresolved {
            return Err("profile attribution categories do not equal the sample denominator".into());
        }
        let valid = self.total - self.invalid_unwind;
        if self.exact_elf + self.memfd_jit + self.unresolved != valid {
            return Err("valid-sample denominator is inconsistent".into());
        }
        Ok(self)
    }
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    match options.action {
        Action::Record(options) => record(options),
        Action::Parse(options) => parse_campaign(&options.results),
    }
}

fn record(options: RecordOptions) -> Result<(), Error> {
    validate_digest(&options.semantic_sha256)?;
    if let Some(digest) = &options.semantic_output_sha256 {
        validate_digest(digest)?;
    }
    let requested_program = options.command.first().ok_or("workload command is empty")?;
    if fs::canonicalize(requested_program)? != fs::canonicalize(&options.executable)? {
        return Err("workload argv[0] must name the declared executable artifact".into());
    }
    fs::create_dir_all(&options.results)?;
    let _lock = campaign_lock(&options.results)?;
    let manifest_path = options.results.join("manifest.json");
    let manifest = if manifest_path.exists() {
        let manifest: Manifest = serde_json::from_reader(File::open(&manifest_path)?)?;
        verify_manifest(&manifest, &options)?;
        manifest
    } else {
        let artifacts = vec![
            freeze("executable", &options.executable, &options.results)?,
            freeze("native-library", &options.native_library, &options.results)?,
            freeze("native-executable", &options.native_executable, &options.results)?,
        ];
        let manifest = Manifest {
            format: FORMAT.into(),
            profiles: PROFILE_COUNT,
            event: "cpu-clock:u".into(),
            frequency_hz: 9_999,
            ring_buffer: PERF_RING_BUFFER.into(),
            call_graph: "dwarf,65528".into(),
            direct_jmp: "off".into(),
            execution_mode: options.execution_mode,
            semantic_sha256: options.semantic_sha256.clone(),
            semantic_output: options.semantic_output.clone(),
            semantic_output_sha256: options.semantic_output_sha256.clone(),
            command: options.command.clone(),
            native_command: options.native_command.clone(),
            artifacts,
        };
        atomic_json(&manifest_path, &manifest)?;
        manifest
    };
    validate_mode_command(&manifest)?;
    let before_path = options.results.join("anchor-before.json");
    if !before_path.exists() {
        atomic_json(&before_path, &run_native_anchor(&manifest, &options.results, "before")?)?;
    }
    let proof_path = options.results.join("mode-proof.json");
    if !proof_path.exists() {
        atomic_json(&proof_path, &run_mode_proof(&manifest, &options.results)?)?;
    }
    let mut receipts = read_receipts(&options.results, &manifest)?;
    for slot in 1..=PROFILE_COUNT {
        if receipts.contains_key(&slot) {
            continue;
        }
        let prefix = options.results.join(format!("profile-{slot:02}"));
        let data = prefix.with_extension("data");
        let stdout = prefix.with_extension("stdout");
        let stderr = prefix.with_extension("stderr");
        let stale_paths = [
            data.clone(),
            stdout.clone(),
            stderr.clone(),
            prefix.with_extension("script.tsv"),
            prefix.with_extension("export.stderr"),
            prefix.with_extension("semantic"),
            prefix.with_extension("symbols.tsv"),
        ];
        for stale in &stale_paths {
            if stale.exists() {
                fs::remove_file(stale)?;
            }
        }
        let output = File::create(&stdout)?;
        let error = File::create(&stderr)?;
        if let Some(path) = &manifest.semantic_output {
            remove_stale_semantic_output(path, &manifest)?;
        }
        let executable = &manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.role == "executable")
            .ok_or("manifest lacks executable")?
            .frozen;
        let native = &manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.role == "native-library")
            .ok_or("manifest lacks native library")?
            .frozen;
        let mut workload = manifest.command.clone();
        workload[0] = executable.to_string_lossy().into_owned();
        let status = Command::new("perf")
            .args([
                "record",
                "--quiet",
                "-e",
                &manifest.event,
                "-F",
                "9999",
                "--call-graph",
                &manifest.call_graph,
                "-m",
                &manifest.ring_buffer,
                "-o",
            ])
            .arg(&data)
            .arg("--")
            .args(&workload)
            .env("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", "1")
            .env_remove("HL_C_DIAGNOSTICS")
            .env(
                "LD_LIBRARY_PATH",
                native.parent().ok_or("native library has no parent")?,
            )
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error))
            .status()?;
        if !status.success() {
            return Err(format!("profile {slot} failed").into());
        }
        if fs::metadata(&stderr)?.len() != 0 {
            return Err(format!("profile {slot} wrote unexpected stderr").into());
        }
        let stdout_hash = sha256(&stdout)?;
        if stdout_hash != manifest.semantic_sha256 {
            return Err(format!("profile {slot} exact semantic hash changed").into());
        }
        let semantic_output_sha256 = if let Some(source) = &manifest.semantic_output {
            let expected = manifest
                .semantic_output_sha256
                .as_ref()
                .ok_or("semantic output digest is absent")?;
            let actual = sha256(source)?;
            if &actual != expected {
                return Err(format!("profile {slot} semantic output hash changed").into());
            }
            let frozen = prefix.with_extension("semantic");
            copy_frozen(source, &frozen)?;
            if sha256(&frozen)? != actual {
                return Err("semantic output changed while frozen".into());
            }
            Some(actual)
        } else {
            None
        };
        let script = prefix.with_extension("script.tsv");
        let exporter_stderr = prefix.with_extension("export.stderr");
        let symbol_index = prefix.with_extension("symbols.tsv");
        freeze_build_ids(&data, &options.results, &symbol_index)?;
        let loss = export_script(&data, &script, &exporter_stderr)?;
        verify_export_loss(slot, &loss)?;
        let samples = count_samples(&script)?;
        if samples == 0 {
            return Err(format!("profile {slot} contains zero cpu-clock:u samples").into());
        }
        let receipt = Receipt {
            slot,
            perf_data_sha256: sha256(&data)?,
            script_sha256: sha256(&script)?,
            exporter_stderr_sha256: sha256(&exporter_stderr)?,
            stdout_sha256: stdout_hash,
            semantic_output_sha256,
            stderr_sha256: sha256(&stderr)?,
            symbol_index_sha256: sha256(&symbol_index)?,
            direct_jmp_ibtc_disabled: true,
            samples,
            lost_records: loss.lost_records,
            lost_chunks: loss.lost_chunks,
            lost_sample_percent: loss.lost_sample_percent,
        };
        append_receipt(&options.results, &receipt)?;
        receipts.insert(slot, receipt);
    }
    let after_path = options.results.join("anchor-after.json");
    if !after_path.exists() {
        atomic_json(&after_path, &run_native_anchor(&manifest, &options.results, "after")?)?;
    }
    seal_campaign(&options.results)?;
    parse_campaign(&options.results)
}

fn remove_stale_semantic_output(path: &Path, manifest: &Manifest) -> Result<(), Error> {
    if manifest
        .artifacts
        .iter()
        .any(|artifact| path == artifact.original || path == artifact.frozen)
    {
        return Err("semantic output cannot overwrite a campaign artifact".into());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_mode_command(manifest: &Manifest) -> Result<(), Error> {
    let translit = manifest.command.iter().any(|arg| arg == "--translit");
    let direct_off = manifest
        .command
        .iter()
        .any(|arg| arg == "--translit-direct-jmp-ibtc=off");
    match manifest.execution_mode {
        ExecutionMode::Interpreter if !translit && !direct_off => Ok(()),
        ExecutionMode::Translit if translit && direct_off => Ok(()),
        ExecutionMode::Interpreter => Err("interpreter campaign command selects transliteration".into()),
        ExecutionMode::Translit => Err("translit campaign lacks --translit and exact direct-JMP-off option".into()),
    }
}

fn exact_semantics(manifest: &Manifest, stdout: &Path) -> Result<Option<String>, Error> {
    if sha256(stdout)? != manifest.semantic_sha256 {
        return Err("control exact stdout hash changed".into());
    }
    match (&manifest.semantic_output, &manifest.semantic_output_sha256) {
        (Some(path), Some(expected)) => {
            let actual = sha256(path)?;
            if &actual != expected {
                return Err("control semantic output hash changed".into());
            }
            Ok(Some(actual))
        }
        (None, None) => Ok(None),
        _ => Err("semantic output contract is incomplete".into()),
    }
}

fn run_native_anchor(manifest: &Manifest, results: &Path, position: &str) -> Result<AnchorReceipt, Error> {
    if !matches!(position, "before" | "after") {
        return Err("native anchor position is invalid".into());
    }
    let native = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.role == "native-executable")
        .ok_or("manifest lacks native executable")?;
    if let Some(path) = &manifest.semantic_output {
        remove_stale_semantic_output(path, manifest)?;
    }
    let stdout = results.join(format!("anchor-{position}.stdout"));
    let stderr = results.join(format!("anchor-{position}.stderr"));
    let status = Command::new(&native.frozen)
        .args(&manifest.native_command)
        .env_remove("HL_C_DIAGNOSTICS")
        .stdout(Stdio::from(File::create(&stdout)?))
        .stderr(Stdio::from(File::create(&stderr)?))
        .status()?;
    if !status.success() {
        return Err(format!("native {position} anchor failed").into());
    }
    let semantic_output_sha256 = exact_semantics(manifest, &stdout)?;
    Ok(AnchorReceipt {
        position: position.into(),
        executable_sha256: native.sha256.clone(),
        stdout_sha256: sha256(&stdout)?,
        stderr_sha256: sha256(&stderr)?,
        semantic_output_sha256,
    })
}

fn run_mode_proof(manifest: &Manifest, results: &Path) -> Result<ModeProof, Error> {
    let executable = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.role == "executable")
        .ok_or("manifest lacks executable")?;
    let native = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.role == "native-library")
        .ok_or("manifest lacks native library")?;
    if let Some(path) = &manifest.semantic_output {
        remove_stale_semantic_output(path, manifest)?;
    }
    let stdout = results.join("mode-proof.stdout");
    let stderr = results.join("mode-proof.stderr");
    let command = mode_proof_command(manifest, &executable.frozen);
    let status = Command::new(&command[0])
        .args(&command[1..])
        .env("HL_C_DIAGNOSTICS", "1")
        .env("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", "1")
        .env(
            "LD_LIBRARY_PATH",
            native.frozen.parent().ok_or("native library has no parent")?,
        )
        .stdout(Stdio::from(File::create(&stdout)?))
        .stderr(Stdio::from(File::create(&stderr)?))
        .status()?;
    if !status.success() {
        return Err("execution-mode proof command failed".into());
    }
    verify_mode_diagnostic(manifest.execution_mode, &fs::read_to_string(&stderr)?)?;
    let semantic_output_sha256 = exact_semantics(manifest, &stdout)?;
    Ok(ModeProof {
        execution_mode: manifest.execution_mode,
        stderr_sha256: sha256(&stderr)?,
        stdout_sha256: sha256(&stdout)?,
        semantic_output_sha256,
    })
}

fn mode_proof_command(manifest: &Manifest, executable: &Path) -> Vec<String> {
    let mut command = manifest.command.clone();
    command[0] = executable.to_string_lossy().into_owned();
    command.insert(1, "--diagnostics".into());
    command
}

fn verify_mode_diagnostic(mode: ExecutionMode, stderr: &str) -> Result<(), Error> {
    let reports = stderr
        .lines()
        .filter(|line| line.starts_with("[prof] translit:"))
        .collect::<Vec<_>>();
    match mode {
        ExecutionMode::Interpreter if reports == ["[prof] translit: not selected"] => Ok(()),
        ExecutionMode::Translit
            if reports.len() == 1
                && reports[0] != "[prof] translit: not selected"
                && stderr
                    .split_whitespace()
                    .any(|field| field == "direct_jmp_ibtc_enabled=0") =>
        {
            Ok(())
        }
        _ => Err("runtime diagnostic does not prove the immutable execution mode".into()),
    }
}

fn seal_campaign(results: &Path) -> Result<(), Error> {
    let seal = FinalSeal {
        format: FORMAT.into(),
        manifest_sha256: sha256(&results.join("manifest.json"))?,
        ledger_sha256: sha256(&results.join("ledger.jsonl"))?,
        proof_sha256: sha256(&results.join("mode-proof.json"))?,
        anchor_before_sha256: sha256(&results.join("anchor-before.json"))?,
        anchor_after_sha256: sha256(&results.join("anchor-after.json"))?,
    };
    atomic_json(&results.join("final.json"), &seal)
}

fn campaign_lock(results: &Path) -> Result<File, Error> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(results.join("campaign.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn validate_artifact_roles(artifacts: &[Frozen]) -> Result<(), Error> {
    let roles = artifacts
        .iter()
        .map(|artifact| artifact.role.as_str())
        .collect::<BTreeSet<_>>();
    if artifacts.len() != 3 || roles != BTreeSet::from(["executable", "native-executable", "native-library"]) {
        return Err("campaign must contain exactly one engine, native executable, and native library artifact".into());
    }
    Ok(())
}

fn verify_manifest(manifest: &Manifest, options: &RecordOptions) -> Result<(), Error> {
    validate_sealed_manifest(manifest)?;
    if manifest.format != FORMAT
        || manifest.profiles != PROFILE_COUNT
        || manifest.command != options.command
        || manifest.semantic_sha256 != options.semantic_sha256
        || manifest.semantic_output != options.semantic_output
        || manifest.semantic_output_sha256 != options.semantic_output_sha256
        || manifest.event != "cpu-clock:u"
        || manifest.frequency_hz != 9_999
        || manifest.ring_buffer != PERF_RING_BUFFER
        || manifest.call_graph != "dwarf,65528"
        || manifest.direct_jmp != "off"
        || manifest.execution_mode != options.execution_mode
        || manifest.native_command != options.native_command
    {
        return Err("resume request does not match the immutable campaign manifest".into());
    }
    Ok(())
}

fn validate_sealed_manifest(manifest: &Manifest) -> Result<(), Error> {
    validate_measurement_contract(manifest)?;
    validate_artifact_roles(&manifest.artifacts)?;
    let executable = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.role == "executable")
        .unwrap();
    if Path::new(&manifest.command[0]) != executable.original {
        return Err("campaign command does not name its executable artifact".into());
    }
    for artifact in &manifest.artifacts {
        validate_digest(&artifact.sha256)?;
        if sha256(&artifact.frozen)? != artifact.sha256 || elf_build_id(&artifact.frozen)? != artifact.build_id {
            return Err(format!("frozen {} identity changed", artifact.role).into());
        }
    }
    Ok(())
}

fn validate_measurement_contract(manifest: &Manifest) -> Result<(), Error> {
    if manifest.format != FORMAT
        || manifest.profiles != PROFILE_COUNT
        || manifest.event != "cpu-clock:u"
        || manifest.frequency_hz != 9_999
        || manifest.ring_buffer != PERF_RING_BUFFER
        || manifest.call_graph != "dwarf,65528"
        || manifest.direct_jmp != "off"
    {
        return Err("campaign measurement contract changed".into());
    }
    validate_digest(&manifest.semantic_sha256)?;
    match (&manifest.semantic_output, &manifest.semantic_output_sha256) {
        (Some(_), Some(digest)) => validate_digest(digest)?,
        (None, None) => {}
        _ => return Err("semantic output path and digest must be paired".into()),
    }
    if manifest.command.is_empty() {
        return Err("campaign command is empty".into());
    }
    Ok(())
}

fn freeze(role: &str, source: &Path, results: &Path) -> Result<Frozen, Error> {
    let original = fs::canonicalize(source)?;
    let digest = sha256(&original)?;
    let build_id = elf_build_id(&original)?;
    let directory = results.join("artifacts").join(role);
    fs::create_dir_all(&directory)?;
    let name = original.file_name().ok_or("artifact has no filename")?;
    let frozen = directory.join(name);
    if frozen.exists() {
        fs::remove_file(&frozen)?;
    }
    copy_frozen(&original, &frozen)?;
    if sha256(&frozen)? != digest || elf_build_id(&frozen)? != build_id {
        return Err("artifact changed while it was frozen".into());
    }
    Ok(Frozen {
        role: role.into(),
        original,
        frozen,
        sha256: digest,
        build_id,
    })
}

fn elf_build_id(path: &Path) -> Result<String, Error> {
    let output = Command::new("readelf").args(["-n"]).arg(path).output()?;
    if !output.status.success() {
        return Err(format!("readelf failed for {}", path.display()).into());
    }
    let text = String::from_utf8(output.stdout)?;
    let id = text
        .lines()
        .find_map(|line| line.split_once("Build ID:").map(|(_, id)| id.trim().to_owned()));
    id.filter(|id| !id.is_empty())
        .ok_or_else(|| format!("{} has no ELF build ID", path.display()).into())
}

fn export_script(data: &Path, destination: &Path, stderr_path: &Path) -> Result<ExportLoss, Error> {
    let output = Command::new("perf")
        .args([
            "script",
            "--no-demangle",
            "--show-lost-events",
            "-G",
            "-F",
            "event,ip,sym,dso",
            "-i",
        ])
        .arg(data)
        .output()?;
    fs::write(stderr_path, &output.stderr)?;
    if !output.status.success() {
        return Err("perf script failed".into());
    }
    let mut loss = parse_export_loss(&String::from_utf8(output.stderr)?)?;
    let (normalized, lost_records) = normalize_perf_script(&String::from_utf8(output.stdout)?)?;
    loss.lost_records = lost_records;
    fs::write(destination, normalized)?;
    Ok(loss)
}

fn parse_export_loss(stderr: &str) -> Result<ExportLoss, Error> {
    if stderr.is_empty() {
        return Ok(ExportLoss::default());
    }
    let mut chunks = None;
    let mut percent = None;
    for line in stderr.lines().filter(|line| !line.is_empty()) {
        if line == "Warning:" || line == "Check IO/CPU overload!" {
            continue;
        }
        if let Some((_, value)) = line
            .strip_prefix("Processed ")
            .and_then(|line| line.split_once(" events and lost "))
        {
            let value = value.strip_suffix(" chunks!").ok_or("malformed lost-chunk report")?;
            if chunks.replace(value.parse()?).is_some() {
                return Err("duplicate lost-chunk report".into());
            }
            continue;
        }
        if let Some((_, value)) = line
            .strip_prefix("Processed ")
            .and_then(|line| line.split_once(" samples and lost "))
        {
            let value = value.strip_suffix("%!").ok_or("malformed lost-sample report")?;
            if percent.replace(value.parse()?).is_some() {
                return Err("duplicate lost-sample report".into());
            }
            continue;
        }
        return Err(format!("unexpected perf exporter diagnostic: {line}").into());
    }
    Ok(ExportLoss {
        lost_records: 0,
        lost_chunks: chunks.ok_or("perf exporter omitted lost-chunk count")?,
        lost_sample_percent: percent.ok_or("perf exporter omitted lost-sample percentage")?,
    })
}

fn verify_export_loss(slot: usize, loss: &ExportLoss) -> Result<(), Error> {
    if loss.lost_records != 0 || loss.lost_chunks != 0 || loss.lost_sample_percent >= MAX_LOST_SAMPLE_PERCENT {
        return Err(format!(
            "profile {slot} exceeded loss threshold: {} records, {} chunks, {:.2}% samples",
            loss.lost_records, loss.lost_chunks, loss.lost_sample_percent
        )
        .into());
    }
    Ok(())
}

fn normalize_perf_script(output: &str) -> Result<(Vec<u8>, u64), Error> {
    let mut normalized = Vec::new();
    let mut lost_records = 0u64;
    for (index, line) in output.lines().enumerate() {
        if let Some(value) = line.strip_prefix("PERF_RECORD_LOST lost ") {
            lost_records = lost_records
                .checked_add(value.parse()?)
                .ok_or("lost-record count overflow")?;
            continue;
        }
        let mut fields = line.split_whitespace();
        let event = fields
            .next()
            .ok_or_else(|| format!("perf script row {} lacks event", index + 1))?;
        let ip = fields
            .next()
            .ok_or_else(|| format!("perf script row {} lacks ip", index + 1))?;
        let symbol = fields
            .next()
            .ok_or_else(|| format!("perf script row {} lacks symbol", index + 1))?;
        let dso = fields.collect::<Vec<_>>().join(" ");
        if dso.is_empty() {
            return Err(format!("perf script row {} lacks dso", index + 1).into());
        }
        let dso = dso
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .ok_or_else(|| format!("perf script row {} has unbound dso", index + 1))?;
        writeln!(normalized, "{event}\t{ip}\t{symbol}\t{dso}")?;
    }
    Ok((normalized, lost_records))
}

fn freeze_build_ids(data: &Path, results: &Path, index_path: &Path) -> Result<(), Error> {
    let output = Command::new("perf").args(["buildid-list", "-i"]).arg(data).output()?;
    if !output.status.success() {
        return Err("perf buildid-list failed".into());
    }
    let root = results.join("symbols");
    fs::create_dir_all(&root)?;
    let mut index = tempfile::NamedTempFile::new_in(results)?;
    for line in String::from_utf8(output.stdout)?.lines() {
        let Some((id, path)) = line.trim().split_once(' ') else {
            continue;
        };
        let path = Path::new(path.trim());
        if !path.is_file() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let destination = root.join(id);
        if !destination.exists() {
            copy_frozen(path, &destination)?;
        }
        if sha256(&destination)? != sha256(path)? || elf_build_id(&destination)? != id {
            return Err(format!("copied ELF identity changed for {}", path.display()).into());
        }
        writeln!(
            index,
            "{id}\t{}\t{}\t{}",
            sha256(&destination)?,
            path.display(),
            destination.display()
        )?;
    }
    index.flush()?;
    index.as_file().sync_all()?;
    index.persist(index_path).map_err(|error| error.error)?;
    sync_directory(results)?;
    Ok(())
}

fn parse_campaign(results: &Path) -> Result<(), Error> {
    let manifest: Manifest = serde_json::from_reader(File::open(results.join("manifest.json"))?)?;
    validate_sealed_manifest(&manifest)?;
    verify_final_seal(results, &manifest)?;
    let receipts = read_receipts(results, &manifest)?;
    if receipts.len() != PROFILE_COUNT {
        return Err("six complete profile receipts are required".into());
    }
    let artifact_dsos = manifest
        .artifacts
        .iter()
        .flat_map(|artifact| [&artifact.original, &artifact.frozen])
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let mut aggregate = Counts::default();
    let mut report =
        String::from("slot\ttotal\texact_elf\tmemfd_jit\tinvalid_unwind\tunresolved\tvalid\tclassified_pct\n");
    for slot in 1..=PROFILE_COUNT {
        let receipt = &receipts[&slot];
        let script = results.join(format!("profile-{slot:02}.script.tsv"));
        let symbol_index = results.join(format!("profile-{slot:02}.symbols.tsv"));
        verify_bound_hash(&symbol_index, &receipt.symbol_index_sha256, "symbol index")?;
        let mut exact_dsos = artifact_dsos.clone();
        exact_dsos.extend(load_symbol_index(&symbol_index)?);
        let counts = parse_script(&script, &exact_dsos)?.verify()?;
        if counts.total != receipt.samples {
            return Err(format!("profile {slot} sample denominator changed").into());
        }
        add(&mut aggregate, counts);
        let valid = counts.total - counts.invalid_unwind;
        let pct = if valid == 0 {
            0.0
        } else {
            100.0 * (counts.exact_elf + counts.memfd_jit) as f64 / valid as f64
        };
        report.push_str(&format!(
            "{slot}\t{}\t{}\t{}\t{}\t{}\t{valid}\t{pct:.3}\n",
            counts.total, counts.exact_elf, counts.memfd_jit, counts.invalid_unwind, counts.unresolved
        ));
    }
    aggregate.verify()?;
    fs::write(results.join("attribution.tsv"), report)?;
    atomic_json(&results.join("attribution.json"), &aggregate)?;
    Ok(())
}

fn verify_final_seal(results: &Path, manifest: &Manifest) -> Result<(), Error> {
    let seal: FinalSeal = serde_json::from_reader(File::open(results.join("final.json"))?)?;
    if seal.format != FORMAT
        || seal.manifest_sha256 != sha256(&results.join("manifest.json"))?
        || seal.ledger_sha256 != sha256(&results.join("ledger.jsonl"))?
        || seal.proof_sha256 != sha256(&results.join("mode-proof.json"))?
        || seal.anchor_before_sha256 != sha256(&results.join("anchor-before.json"))?
        || seal.anchor_after_sha256 != sha256(&results.join("anchor-after.json"))?
    {
        return Err("final campaign seal does not bind its evidence".into());
    }
    let before: AnchorReceipt = serde_json::from_reader(File::open(results.join("anchor-before.json"))?)?;
    let after: AnchorReceipt = serde_json::from_reader(File::open(results.join("anchor-after.json"))?)?;
    let native = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.role == "native-executable")
        .ok_or("manifest lacks native executable")?;
    verify_anchor_receipts(&before, &after, native, manifest)?;
    let proof: ModeProof = serde_json::from_reader(File::open(results.join("mode-proof.json"))?)?;
    if proof.execution_mode != manifest.execution_mode
        || proof.stdout_sha256 != manifest.semantic_sha256
        || proof.semantic_output_sha256 != manifest.semantic_output_sha256
        || proof.stderr_sha256 != sha256(&results.join("mode-proof.stderr"))?
    {
        return Err("execution-mode proof receipt changed".into());
    }
    verify_mode_diagnostic(
        manifest.execution_mode,
        &fs::read_to_string(results.join("mode-proof.stderr"))?,
    )
}

fn verify_anchor_receipts(
    before: &AnchorReceipt,
    after: &AnchorReceipt,
    native: &Frozen,
    manifest: &Manifest,
) -> Result<(), Error> {
    for (anchor, position) in [(before, "before"), (after, "after")] {
        if anchor.position != position
            || anchor.executable_sha256 != native.sha256
            || anchor.stdout_sha256 != manifest.semantic_sha256
            || anchor.stderr_sha256 != EMPTY_SHA256
            || anchor.semantic_output_sha256 != manifest.semantic_output_sha256
        {
            return Err(format!("native {position} anchor moved or changed semantics").into());
        }
    }
    Ok(())
}

fn parse_script(path: &Path, exact_dsos: &BTreeSet<String>) -> Result<Counts, Error> {
    let mut counts = Counts::default();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err(format!("malformed perf-script row: {line}").into());
        }
        let [event, ip, symbol, dso] = <[&str; 4]>::try_from(fields).unwrap();
        if event != "cpu-clock:u:" {
            continue;
        }
        counts.total += 1;
        let ip = ip.trim_start_matches("0x");
        if ip.is_empty()
            || ip.bytes().all(|byte| byte == b'0')
            || ip.eq_ignore_ascii_case("ffffffffffffffff")
            || !ip.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            counts.invalid_unwind += 1;
        } else if matches!(dso, "/memfd:hl-code" | "/memfd:hl-code (deleted)") {
            counts.memfd_jit += 1;
        } else if exact_dsos.contains(dso) && !matches!(symbol, "[unknown]" | "[.]" | "0") {
            counts.exact_elf += 1;
        } else {
            counts.unresolved += 1;
        }
    }
    counts.verify()
}

fn count_samples(path: &Path) -> Result<u64, Error> {
    Ok(BufReader::new(File::open(path)?).lines().try_fold(0, |count, line| {
        let line = line?;
        Ok::<_, std::io::Error>(count + u64::from(line.split('\t').next() == Some("cpu-clock:u:")))
    })?)
}

fn read_receipts(results: &Path, manifest: &Manifest) -> Result<BTreeMap<usize, Receipt>, Error> {
    let path = results.join("ledger.jsonl");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let mut receipts = BTreeMap::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let receipt: Receipt = serde_json::from_str(&line?)?;
        if receipt.samples == 0 {
            return Err("ledger contains a zero-sample profile".into());
        }
        if !(1..=manifest.profiles).contains(&receipt.slot) || receipts.insert(receipt.slot, receipt).is_some() {
            return Err("ledger contains a duplicate or foreign profile slot".into());
        }
    }
    for receipt in receipts.values() {
        let prefix = results.join(format!("profile-{:02}", receipt.slot));
        if receipt.stdout_sha256 != manifest.semantic_sha256
            || receipt.semantic_output_sha256 != manifest.semantic_output_sha256
            || sha256(&prefix.with_extension("data"))? != receipt.perf_data_sha256
            || sha256(&prefix.with_extension("script.tsv"))? != receipt.script_sha256
            || sha256(&prefix.with_extension("export.stderr"))? != receipt.exporter_stderr_sha256
            || sha256(&prefix.with_extension("stdout"))? != receipt.stdout_sha256
            || sha256(&prefix.with_extension("stderr"))? != receipt.stderr_sha256
            || sha256(&prefix.with_extension("symbols.tsv"))? != receipt.symbol_index_sha256
            || !receipt.direct_jmp_ibtc_disabled
            || receipt.lost_records != 0
            || receipt.lost_chunks != 0
            || receipt.lost_sample_percent >= MAX_LOST_SAMPLE_PERCENT
            || match &receipt.semantic_output_sha256 {
                Some(digest) => sha256(&prefix.with_extension("semantic"))? != *digest,
                None => false,
            }
        {
            return Err(format!("profile {} receipt no longer matches its files", receipt.slot).into());
        }
    }
    Ok(receipts)
}

fn append_receipt(results: &Path, receipt: &Receipt) -> Result<(), Error> {
    let path = results.join("ledger.jsonl");
    let mut file = tempfile::NamedTempFile::new_in(results)?;
    if path.exists() {
        std::io::copy(&mut File::open(&path)?, &mut file)?;
    }
    serde_json::to_writer(&mut file, receipt)?;
    writeln!(file)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    sync_directory(results)?;
    Ok(())
}

fn load_symbol_index(path: &Path) -> Result<BTreeSet<String>, Error> {
    let mut dsos = BTreeSet::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err("malformed frozen symbol index".into());
        }
        let frozen = Path::new(fields[3]);
        if sha256(frozen)? != fields[1] || elf_build_id(frozen)? != fields[0] {
            return Err(format!("frozen symbol identity changed: {}", frozen.display()).into());
        }
        dsos.insert(fields[2].into());
        dsos.insert(fields[3].into());
    }
    Ok(dsos)
}

fn add(total: &mut Counts, value: Counts) {
    total.total += value.total;
    total.exact_elf += value.exact_elf;
    total.memfd_jit += value.memfd_jit;
    total.invalid_unwind += value.invalid_unwind;
    total.unresolved += value.unresolved;
}

fn sha256(path: &Path) -> Result<String, Error> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        use std::io::Read;
        let length = input.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    Ok(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn verify_bound_hash(path: &Path, expected: &str, label: &str) -> Result<(), Error> {
    if sha256(path)? != expected {
        return Err(format!("{label} changed").into());
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("semantic SHA-256 must be 64 hexadecimal characters".into());
    }
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let parent = path.parent().ok_or("publication path has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    sync_directory(parent)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), Error> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn copy_frozen(source: &Path, destination: &Path) -> Result<(), Error> {
    let parent = destination.parent().ok_or("frozen copy has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut File::open(source)?, &mut temporary)?;
    temporary
        .as_file()
        .set_permissions(fs::metadata(source)?.permissions())?;
    temporary.as_file().sync_all()?;
    temporary.persist(destination).map_err(|error| error.error)?;
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(text: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("script.tsv");
        fs::write(&path, text).unwrap();
        (directory, path)
    }

    #[test]
    fn normalizes_supported_perf_script_grammar_without_losing_dso_spaces() {
        let output = concat!(
            "cpu-clock:u:      60918e9763a1 run_guest (/frozen/hl)\n",
            "cpu-clock:u:      7c505da05ac7 [.] (/memfd:hl-code (deleted))\n",
        );
        assert_eq!(
            String::from_utf8(normalize_perf_script(output).unwrap().0).unwrap(),
            concat!(
                "cpu-clock:u:\t60918e9763a1\trun_guest\t/frozen/hl\n",
                "cpu-clock:u:\t7c505da05ac7\t[.]\t/memfd:hl-code (deleted)\n",
            )
        );
        assert!(normalize_perf_script("cpu-clock:u: malformed\n").is_err());
        let (_, lost) = normalize_perf_script("PERF_RECORD_LOST lost 7\nPERF_RECORD_LOST lost 5\n").unwrap();
        assert_eq!(lost, 12);
        assert!(verify_export_loss(
            1,
            &ExportLoss {
                lost_records: lost,
                ..ExportLoss::default()
            }
        )
        .is_err());
    }

    #[test]
    fn exporter_loss_is_parsed_sealed_and_bounded() {
        assert_eq!(parse_export_loss("").unwrap(), ExportLoss::default());
        let warning = concat!(
            "Warning:\n",
            "Processed 4589 events and lost 30 chunks!\n\n",
            "Check IO/CPU overload!\n\n",
            "Warning:\n",
            "Processed 3315 samples and lost 8.14%!\n",
        );
        let loss = parse_export_loss(warning).unwrap();
        assert_eq!(loss.lost_chunks, 30);
        assert_eq!(loss.lost_records, 0);
        assert_eq!(loss.lost_sample_percent, 8.14);
        assert!(verify_export_loss(1, &loss).is_err());
        assert!(
            verify_export_loss(
                1,
                &ExportLoss {
                    lost_chunks: 0,
                    lost_records: 0,
                    lost_sample_percent: 0.99,
                }
            )
            .is_ok()
        );
        assert!(parse_export_loss(&warning.replace("8.14%", "unknown")).is_err());
        assert!(parse_export_loss(&warning.replace("30 chunks!", "")).is_err());
        assert!(parse_export_loss(&(warning.to_owned() + "spoofed diagnostic\n")).is_err());
    }

    #[test]
    fn partitions_exact_elf_memfd_invalid_and_unresolved_samples() {
        let (_directory, path) = script(concat!(
            "cpu-clock:u:\t0000000000401000\trun_guest\t/frozen/hl\n",
            "cpu-clock:u:\t00007f0100001000\t[.]\t/memfd:hl-code (deleted)\n",
            "cpu-clock:u:\tffffffffffffffff\t[unknown]\t[unknown]\n",
            "cpu-clock:u:\t0000000000402000\t[unknown]\t/frozen/hl\n",
            "cpu-clock:u:\t00007f0100002000\t[.]\t/memfd:hl-code-spoof (deleted)\n",
            "cpu-clock:k:\t0000000000401000\trun_guest\t/frozen/hl\n",
            "mmap:\t0000000000000000\tignored\t/memfd:hl-code\n",
        ));
        let counts = parse_script(&path, &BTreeSet::from(["/frozen/hl".into()])).unwrap();
        assert_eq!(
            counts,
            Counts {
                total: 5,
                exact_elf: 1,
                memfd_jit: 1,
                invalid_unwind: 1,
                unresolved: 2
            }
        );
    }

    #[test]
    fn malformed_rows_and_inconsistent_denominators_are_rejected() {
        let (_directory, path) = script("cpu-clock:u:\tbad\n");
        assert!(parse_script(&path, &BTreeSet::new()).is_err());
        assert!(
            Counts {
                total: 4,
                exact_elf: 1,
                memfd_jit: 1,
                invalid_unwind: 0,
                unresolved: 1
            }
            .verify()
            .is_err()
        );
    }

    #[test]
    fn ledger_rejects_duplicate_and_foreign_slots() {
        let directory = tempfile::tempdir().unwrap();
        let receipt = Receipt {
            slot: 7,
            perf_data_sha256: "a".into(),
            script_sha256: "b".into(),
            exporter_stderr_sha256: EMPTY_SHA256.into(),
            stdout_sha256: "c".into(),
            semantic_output_sha256: None,
            stderr_sha256: "d".into(),
            symbol_index_sha256: "e".into(),
            direct_jmp_ibtc_disabled: true,
            samples: 1,
            lost_records: 0,
            lost_chunks: 0,
            lost_sample_percent: 0.0,
        };
        append_receipt(directory.path(), &receipt).unwrap();
        let manifest = Manifest {
            format: FORMAT.into(),
            profiles: PROFILE_COUNT,
            event: "cpu-clock:u".into(),
            frequency_hz: 9_999,
            ring_buffer: PERF_RING_BUFFER.into(),
            call_graph: "dwarf,65528".into(),
            direct_jmp: "off".into(),
            execution_mode: ExecutionMode::Interpreter,
            semantic_sha256: "0".repeat(64),
            semantic_output: None,
            semantic_output_sha256: None,
            command: vec!["x".into()],
            native_command: vec!["root".into(), "gcc".into()],
            artifacts: vec![],
        };
        assert!(
            read_receipts(directory.path(), &manifest)
                .unwrap_err()
                .to_string()
                .contains("foreign")
        );
    }

    #[test]
    fn campaign_lock_excludes_a_concurrent_writer() {
        let directory = tempfile::tempdir().unwrap();
        let _held = campaign_lock(directory.path()).unwrap();
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.path().join("campaign.lock"))
            .unwrap();
        assert!(contender.try_lock_exclusive().is_err());
    }

    #[test]
    fn execution_mode_diagnostics_reject_mismatch_and_spoofs() {
        verify_mode_diagnostic(ExecutionMode::Interpreter, "[prof] translit: not selected\n").unwrap();
        assert!(verify_mode_diagnostic(ExecutionMode::Translit, "[prof] translit: not selected\n").is_err());
        assert!(
            verify_mode_diagnostic(
                ExecutionMode::Translit,
                "[prof] translit: blocks=2\ndirect_jmp_ibtc_enabled=00\n"
            )
            .is_err()
        );
        verify_mode_diagnostic(
            ExecutionMode::Translit,
            "[prof] translit: blocks=2\n[diag] direct_jmp_ibtc_enabled=0\n",
        )
        .unwrap();
    }

    #[test]
    fn mode_proof_argv_enables_diagnostics_without_changing_measured_argv() {
        let manifest = test_manifest(vec![]);
        let proof = mode_proof_command(&manifest, Path::new("/frozen/hl"));
        assert_eq!(proof, ["/frozen/hl", "--diagnostics"]);
        assert_eq!(manifest.command, ["x"]);
    }

    #[test]
    fn sealed_symbol_index_rejects_a_post_receipt_edit() {
        let (_directory, path) = script("build-id\tsha\toriginal\tfrozen\n");
        let sealed = sha256(&path).unwrap();
        verify_bound_hash(&path, &sealed, "symbol index").unwrap();
        fs::write(&path, "spoofed\n").unwrap();
        assert!(verify_bound_hash(&path, &sealed, "symbol index").is_err());
    }

    #[test]
    fn sealed_exporter_stderr_rejects_dropped_or_tampered_warning() {
        let (_directory, path) = script("Warning:\nProcessed 1 events and lost 1 chunks!\n");
        let sealed = sha256(&path).unwrap();
        fs::write(&path, "").unwrap();
        assert!(verify_bound_hash(&path, &sealed, "exporter stderr").is_err());
        fs::write(&path, "Warning: spoofed\n").unwrap();
        assert!(verify_bound_hash(&path, &sealed, "exporter stderr").is_err());
    }

    #[test]
    fn manifest_contract_rejects_event_frequency_ring_unwind_and_direct_jmp_changes() {
        let mut manifest = test_manifest(vec![]);
        validate_measurement_contract(&manifest).unwrap();
        manifest.event = "cycles:u".into();
        assert!(validate_measurement_contract(&manifest).is_err());
        manifest.event = "cpu-clock:u".into();
        manifest.frequency_hz = 10_000;
        assert!(validate_measurement_contract(&manifest).is_err());
        manifest.frequency_hz = 9_999;
        manifest.ring_buffer = "8M".into();
        assert!(validate_measurement_contract(&manifest).is_err());
        manifest.ring_buffer = PERF_RING_BUFFER.into();
        manifest.call_graph = "fp".into();
        assert!(validate_measurement_contract(&manifest).is_err());
        manifest.call_graph = "dwarf,65528".into();
        manifest.direct_jmp = "on".into();
        assert!(validate_measurement_contract(&manifest).is_err());
    }

    #[test]
    fn atomic_publication_ignores_an_abandoned_unique_temporary() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("abandoned.tmp"), b"partial").unwrap();
        let path = directory.path().join("complete.json");
        atomic_json(&path, &serde_json::json!({"complete": true})).unwrap();
        assert_eq!(
            serde_json::from_reader::<_, serde_json::Value>(File::open(path).unwrap()).unwrap()["complete"],
            true
        );
    }

    #[cfg(unix)]
    #[test]
    fn frozen_copy_preserves_executable_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("frozen");
        fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o751)).unwrap();
        copy_frozen(&source, &destination).unwrap();
        assert_eq!(fs::metadata(destination).unwrap().permissions().mode() & 0o777, 0o751);
        assert!(
            Command::new(directory.path().join("frozen"))
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn stale_semantic_output_is_removed_and_artifacts_are_protected() {
        let directory = tempfile::tempdir().unwrap();
        let stale = directory.path().join("answer.o");
        fs::write(&stale, b"stale").unwrap();
        let artifact = directory.path().join("hl");
        fs::write(&artifact, b"elf").unwrap();
        let manifest = test_manifest(vec![
            frozen("executable", &artifact),
            frozen("native-library", &directory.path().join("lib.so")),
        ]);
        remove_stale_semantic_output(&stale, &manifest).unwrap();
        assert!(!stale.exists());
        assert!(remove_stale_semantic_output(&artifact, &manifest).is_err());
    }

    #[test]
    fn duplicate_roles_and_zero_sample_receipts_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("hl");
        let duplicate = vec![frozen("executable", &artifact), frozen("executable", &artifact)];
        assert!(validate_artifact_roles(&duplicate).is_err());
        let receipt = Receipt {
            slot: 1,
            perf_data_sha256: "a".into(),
            script_sha256: "b".into(),
            exporter_stderr_sha256: EMPTY_SHA256.into(),
            stdout_sha256: "c".into(),
            semantic_output_sha256: None,
            stderr_sha256: "d".into(),
            symbol_index_sha256: "e".into(),
            direct_jmp_ibtc_disabled: true,
            samples: 0,
            lost_records: 0,
            lost_chunks: 0,
            lost_sample_percent: 0.0,
        };
        append_receipt(directory.path(), &receipt).unwrap();
        assert!(
            read_receipts(directory.path(), &test_manifest(vec![]))
                .unwrap_err()
                .to_string()
                .contains("zero-sample")
        );
    }

    #[test]
    fn native_anchors_cannot_be_missing_or_moved() {
        let native = frozen("native-executable", Path::new("/native"));
        let manifest = test_manifest(vec![]);
        let anchor = |position: &str| AnchorReceipt {
            position: position.into(),
            executable_sha256: native.sha256.clone(),
            stdout_sha256: manifest.semantic_sha256.clone(),
            stderr_sha256: EMPTY_SHA256.into(),
            semantic_output_sha256: None,
        };
        verify_anchor_receipts(&anchor("before"), &anchor("after"), &native, &manifest).unwrap();
        assert!(verify_anchor_receipts(&anchor("after"), &anchor("before"), &native, &manifest).is_err());
    }

    fn frozen(role: &str, path: &Path) -> Frozen {
        Frozen {
            role: role.into(),
            original: path.into(),
            frozen: path.into(),
            sha256: "0".repeat(64),
            build_id: "00".into(),
        }
    }

    fn test_manifest(artifacts: Vec<Frozen>) -> Manifest {
        Manifest {
            format: FORMAT.into(),
            profiles: PROFILE_COUNT,
            event: "cpu-clock:u".into(),
            frequency_hz: 9_999,
            ring_buffer: PERF_RING_BUFFER.into(),
            call_graph: "dwarf,65528".into(),
            direct_jmp: "off".into(),
            execution_mode: ExecutionMode::Interpreter,
            semantic_sha256: "0".repeat(64),
            semantic_output: None,
            semantic_output_sha256: None,
            command: vec!["x".into()],
            native_command: vec!["root".into(), "gcc".into()],
            artifacts,
        }
    }
}
