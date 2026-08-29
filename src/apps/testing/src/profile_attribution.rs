//! Bounded CPU-profile acquisition and deterministic offline attribution.

use clap::{Args, Subcommand};
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
    call_graph: String,
    direct_jmp: String,
    semantic_sha256: String,
    semantic_output: Option<PathBuf>,
    semantic_output_sha256: Option<String>,
    command: Vec<String>,
    artifacts: Vec<Frozen>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Receipt {
    slot: usize,
    perf_data_sha256: String,
    script_sha256: String,
    stdout_sha256: String,
    semantic_output_sha256: Option<String>,
    samples: u64,
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
    let manifest_path = options.results.join("manifest.json");
    let manifest = if manifest_path.exists() {
        let manifest: Manifest = serde_json::from_reader(File::open(&manifest_path)?)?;
        verify_manifest(&manifest, &options)?;
        manifest
    } else {
        let artifacts = vec![
            freeze("executable", &options.executable, &options.results)?,
            freeze("native-library", &options.native_library, &options.results)?,
        ];
        let manifest = Manifest {
            format: FORMAT.into(),
            profiles: PROFILE_COUNT,
            event: "cpu-clock:u".into(),
            frequency_hz: 9_999,
            call_graph: "dwarf,65528".into(),
            direct_jmp: "off".into(),
            semantic_sha256: options.semantic_sha256.clone(),
            semantic_output: options.semantic_output.clone(),
            semantic_output_sha256: options.semantic_output_sha256.clone(),
            command: options.command.clone(),
            artifacts,
        };
        atomic_json(&manifest_path, &manifest)?;
        manifest
    };
    let mut receipts = read_receipts(&options.results, &manifest)?;
    for slot in 1..=PROFILE_COUNT {
        if receipts.contains_key(&slot) {
            continue;
        }
        let prefix = options.results.join(format!("profile-{slot:02}"));
        let data = prefix.with_extension("data");
        let stdout = prefix.with_extension("stdout");
        let stderr = prefix.with_extension("stderr");
        let output = File::create(&stdout)?;
        let error = File::create(&stderr)?;
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
                "-o",
            ])
            .arg(&data)
            .arg("--")
            .args(&workload)
            .env("HL_TRANSLIT_DIRECT_JMP_IBTC_DISABLE", "1")
            .env(
                "LD_LIBRARY_PATH",
                native.parent().ok_or("native library has no parent")?,
            )
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(error))
            .status()?;
        if !status.success() || fs::metadata(&stderr)?.len() != 0 {
            return Err(format!("profile {slot} failed or wrote stderr").into());
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
            fs::copy(source, &frozen)?;
            if sha256(&frozen)? != actual {
                return Err("semantic output changed while frozen".into());
            }
            Some(actual)
        } else {
            None
        };
        let script = prefix.with_extension("script.tsv");
        freeze_build_ids(&data, &options.results)?;
        export_script(&data, &script)?;
        let samples = count_samples(&script)?;
        let receipt = Receipt {
            slot,
            perf_data_sha256: sha256(&data)?,
            script_sha256: sha256(&script)?,
            stdout_sha256: stdout_hash,
            semantic_output_sha256,
            samples,
        };
        append_receipt(&options.results, &receipt)?;
        receipts.insert(slot, receipt);
    }
    parse_campaign(&options.results)
}

fn verify_manifest(manifest: &Manifest, options: &RecordOptions) -> Result<(), Error> {
    if manifest.format != FORMAT
        || manifest.profiles != PROFILE_COUNT
        || manifest.command != options.command
        || manifest.semantic_sha256 != options.semantic_sha256
        || manifest.semantic_output != options.semantic_output
        || manifest.semantic_output_sha256 != options.semantic_output_sha256
        || manifest.event != "cpu-clock:u"
        || manifest.frequency_hz != 9_999
        || manifest.call_graph != "dwarf,65528"
        || manifest.direct_jmp != "off"
    {
        return Err("resume request does not match the immutable campaign manifest".into());
    }
    for artifact in &manifest.artifacts {
        if sha256(&artifact.frozen)? != artifact.sha256 || elf_build_id(&artifact.frozen)? != artifact.build_id {
            return Err(format!("frozen {} identity changed", artifact.role).into());
        }
    }
    Ok(())
}

fn freeze(role: &str, source: &Path, results: &Path) -> Result<Frozen, Error> {
    let original = fs::canonicalize(source)?;
    let sha256 = sha256(&original)?;
    let build_id = elf_build_id(&original)?;
    let directory = results.join("artifacts").join(role);
    fs::create_dir_all(&directory)?;
    let name = original.file_name().ok_or("artifact has no filename")?;
    let frozen = directory.join(name);
    fs::copy(&original, &frozen)?;
    if sha256(&frozen)? != sha256 || elf_build_id(&frozen)? != build_id {
        return Err("artifact changed while it was frozen".into());
    }
    Ok(Frozen {
        role: role.into(),
        original,
        frozen,
        sha256,
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

fn export_script(data: &Path, destination: &Path) -> Result<(), Error> {
    let output = Command::new("perf")
        .args([
            "script",
            "--no-demangle",
            "--field-separator",
            "\t",
            "-F",
            "event,ip,sym,dso",
            "-i",
        ])
        .arg(data)
        .output()?;
    if !output.status.success() {
        return Err("perf script failed".into());
    }
    fs::write(destination, output.stdout)?;
    Ok(())
}

fn freeze_build_ids(data: &Path, results: &Path) -> Result<(), Error> {
    let output = Command::new("perf").args(["buildid-list", "-i"]).arg(data).output()?;
    if !output.status.success() {
        return Err("perf buildid-list failed".into());
    }
    let root = results.join("symbols");
    fs::create_dir_all(&root)?;
    let mut index = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("index.tsv"))?;
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
            fs::copy(path, &destination)?;
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
    Ok(())
}

fn parse_campaign(results: &Path) -> Result<(), Error> {
    let manifest: Manifest = serde_json::from_reader(File::open(results.join("manifest.json"))?)?;
    if manifest.format != FORMAT || manifest.profiles != PROFILE_COUNT {
        return Err("not a six-profile attribution campaign".into());
    }
    let receipts = read_receipts(results, &manifest)?;
    if receipts.len() != PROFILE_COUNT {
        return Err("six complete profile receipts are required".into());
    }
    let mut exact_dsos = manifest
        .artifacts
        .iter()
        .flat_map(|artifact| [&artifact.original, &artifact.frozen])
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    exact_dsos.extend(load_symbol_index(results)?);
    let mut aggregate = Counts::default();
    let mut report =
        String::from("slot\ttotal\texact_elf\tmemfd_jit\tinvalid_unwind\tunresolved\tvalid\tclassified_pct\n");
    for slot in 1..=PROFILE_COUNT {
        let receipt = &receipts[&slot];
        let script = results.join(format!("profile-{slot:02}.script.tsv"));
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
        if !event.trim_end_matches(':').starts_with("cpu-clock") {
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
        } else if dso.starts_with("/memfd:hl-code") || dso.starts_with("memfd:hl-code") {
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
        Ok::<_, std::io::Error>(count + u64::from(line.starts_with("cpu-clock")))
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
        if !(1..=manifest.profiles).contains(&receipt.slot) || receipts.insert(receipt.slot, receipt).is_some() {
            return Err("ledger contains a duplicate or foreign profile slot".into());
        }
    }
    for receipt in receipts.values() {
        let prefix = results.join(format!("profile-{:02}", receipt.slot));
        if sha256(&prefix.with_extension("data"))? != receipt.perf_data_sha256
            || sha256(&prefix.with_extension("script.tsv"))? != receipt.script_sha256
            || sha256(&prefix.with_extension("stdout"))? != receipt.stdout_sha256
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
    let temporary = results.join("ledger.jsonl.tmp");
    let mut file = File::create(&temporary)?;
    if path.exists() {
        std::io::copy(&mut File::open(&path)?, &mut file)?;
    }
    serde_json::to_writer(&mut file, receipt)?;
    writeln!(file)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn load_symbol_index(results: &Path) -> Result<BTreeSet<String>, Error> {
    let path = results.join("symbols/index.tsv");
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
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
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_digest(value: &str) -> Result<(), Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("semantic SHA-256 must be 64 hexadecimal characters".into());
    }
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
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
    fn partitions_exact_elf_memfd_invalid_and_unresolved_samples() {
        let (_directory, path) = script(concat!(
            "cpu-clock:u:\t0000000000401000\trun_guest\t/frozen/hl\n",
            "cpu-clock:u:\t00007f0100001000\t[.]\t/memfd:hl-code (deleted)\n",
            "cpu-clock:u:\tffffffffffffffff\t[unknown]\t[unknown]\n",
            "cpu-clock:u:\t0000000000402000\t[unknown]\t/frozen/hl\n",
            "mmap:\t0000000000000000\tignored\t/memfd:hl-code\n",
        ));
        let counts = parse_script(&path, &BTreeSet::from(["/frozen/hl".into()])).unwrap();
        assert_eq!(
            counts,
            Counts {
                total: 4,
                exact_elf: 1,
                memfd_jit: 1,
                invalid_unwind: 1,
                unresolved: 1
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
            stdout_sha256: "c".into(),
            semantic_output_sha256: None,
            samples: 1,
        };
        append_receipt(directory.path(), &receipt).unwrap();
        let manifest = Manifest {
            format: FORMAT.into(),
            profiles: PROFILE_COUNT,
            event: "cpu-clock:u".into(),
            frequency_hz: 9_999,
            call_graph: "dwarf,65528".into(),
            direct_jmp: "off".into(),
            semantic_sha256: "0".repeat(64),
            semantic_output: None,
            semantic_output_sha256: None,
            command: vec!["x".into()],
            artifacts: vec![],
        };
        assert!(
            read_receipts(directory.path(), &manifest)
                .unwrap_err()
                .to_string()
                .contains("foreign")
        );
    }
}
