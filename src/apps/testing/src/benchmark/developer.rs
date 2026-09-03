//! A real edit/build/test session, measured under native and translated execution backends.
//!
//! The checkpoint acceptance plan additionally requires two real capture/restore cycles. They do not
//! belong in this runner until it owns a production checkpoint controller: phase markers alone would be
//! a fake boundary that proves no process state was captured.

use super::{evidence::Measurement, identity};
use crate::suite::Error;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};

const SCHEMA: &str = "husklet-developer-benchmark-v3";
const UNIT_COUNT: u32 = 128;
const PHASES: [&str; 11] = [
    "prompt",
    "git",
    "search",
    "full-build",
    "edit",
    "incremental-build",
    "test",
    "archive",
    "package-metadata",
    "spawn",
    "final",
];
const ORDER: [Mode; 6] = [
    Mode::Native,
    Mode::Supervised,
    Mode::Translated,
    Mode::Translated,
    Mode::Supervised,
    Mode::Native,
];

#[derive(Args)]
pub(crate) struct Options {
    /// Immutable baseline Linux rootfs containing sh, gcc, make, git, rg, tar, and apk metadata.
    #[arg(long)]
    rootfs: PathBuf,
    /// Production worker for the native-supervised baseline arm.
    #[arg(long)]
    engine: PathBuf,
    /// Guest ISA served by the baseline worker and rootfs. Defaults preserve the original x86 campaign.
    #[arg(long, value_enum, default_value = "x86_64")]
    guest_isa: GuestIsa,
    /// Rootfs for the translated arm. Defaults to --rootfs for a same-ISA campaign.
    #[arg(long)]
    translated_rootfs: Option<PathBuf>,
    /// Production worker for the translated arm. Defaults to --engine for a same-ISA campaign.
    #[arg(long)]
    translated_engine: Option<PathBuf>,
    /// Guest ISA served by the translated worker/rootfs. Defaults to --guest-isa.
    #[arg(long, value_enum)]
    translated_guest_isa: Option<GuestIsa>,
    /// Native engine loaded by the runner.
    #[arg(long)]
    native_library: PathBuf,
    /// Fresh result directory, or the exact directory named with --resume.
    #[arg(long)]
    results: PathBuf,
    #[arg(long)]
    resume: bool,
    #[arg(long, default_value_t = 3, value_parser = parse_samples)]
    samples: u32,
    #[arg(long, default_value_t = 30)]
    quiet_seconds: u64,
    #[arg(long, default_value_t = 900)]
    lock_timeout: u64,
    #[arg(long, default_value_t = 1.0)]
    max_load: f64,
}

fn parse_samples(value: &str) -> Result<u32, String> {
    match value.parse() {
        Ok(value @ (3 | 5)) => Ok(value),
        _ => Err("samples must be 3 or 5".into()),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Native,
    Supervised,
    Translated,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum GuestIsa {
    #[value(name = "aarch64", alias = "arm64")]
    Aarch64,
    #[value(name = "x86_64", alias = "amd64")]
    X86_64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HostIsa {
    Aarch64,
    X86_64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ExecutionBackend {
    HostNative,
    NativeSupervised,
    /// The x86 guest translator emits x86 on x86 and AArch64 on ARM hosts.
    X86Transliterator,
    /// The AArch64 guest translator emits AArch64 on an AArch64 host.
    Aarch64Transliterator,
    /// AArch64 guests are interpreter-only on an x86 host today.
    Aarch64Interpreter,
}

impl GuestIsa {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
}

#[derive(Clone, Copy)]
struct ArmArtifacts<'a> {
    rootfs: &'a Path,
    engine: &'a Path,
    guest_isa: GuestIsa,
}

#[derive(Deserialize, Serialize)]
struct Identity {
    schema: String,
    rootfs: String,
    engine: String,
    native_library: String,
    host_isa: HostIsa,
    baseline_guest_isa: GuestIsa,
    translated_rootfs: String,
    translated_engine: String,
    translated_guest_isa: GuestIsa,
    translated_backend: ExecutionBackend,
    workload_recipe: String,
    samples: u32,
}

#[derive(Deserialize, Serialize)]
struct Row {
    sample: u32,
    position: usize,
    mode: Mode,
    host_isa: HostIsa,
    guest_isa: GuestIsa,
    backend: ExecutionBackend,
    wall_ns: u128,
    counters: Counters,
    null_counters: Counters,
    phase_ns: Vec<(String, u128)>,
    stdout_sha256: String,
    semantic_sha256: String,
    portable_semantic_sha256: String,
    backend_receipt: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Counters {
    duration_ns: u64,
    task_clock_ms: f64,
    instructions: u64,
    cycles: u64,
    page_faults: u64,
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    validate_options(&options)?;
    let fixture = fixture_source();
    let translated = arm_artifacts(&options, Mode::Translated);
    let expected = Identity {
        schema: SCHEMA.into(),
        rootfs: identity::artifact_identity(&options.rootfs)?,
        engine: identity::artifact_identity(&options.engine)?,
        native_library: identity::artifact_identity(&options.native_library)?,
        host_isa: host_isa(),
        baseline_guest_isa: options.guest_isa,
        translated_rootfs: identity::artifact_identity(translated.rootfs)?,
        translated_engine: identity::artifact_identity(translated.engine)?,
        translated_guest_isa: translated.guest_isa,
        translated_backend: execution_backend(host_isa(), Mode::Translated, translated.guest_isa),
        workload_recipe: workload_recipe_identity(),
        samples: options.samples,
    };
    let ledger_path = options.results.join("ledger.jsonl");
    if options.resume {
        let recorded: Identity = serde_json::from_reader(File::open(options.results.join("identity.json"))?)?;
        require_identity(&recorded, &expected)?;
    } else {
        if options.results.exists() {
            return Err("results directory already exists; use --resume only for the exact campaign".into());
        }
        fs::create_dir_all(&options.results)?;
        atomic_json(&options.results.join("identity.json"), &expected)?;
    }
    let completed = read_ledger(&ledger_path, options.samples)?;
    let measurement = Measurement::acquire(options.quiet_seconds, options.lock_timeout, options.max_load)?;
    fs::write(options.results.join("lock.receipt"), measurement.receipt())?;
    for sample in 0..options.samples {
        for (position, mode) in ORDER.into_iter().enumerate() {
            if completed
                .iter()
                .any(|row| row.sample == sample && row.position == position)
            {
                continue;
            }
            let root = options.results.join(format!("root-{sample}-{position}"));
            if root.exists() {
                fs::remove_dir_all(&root)?;
            }
            clone_root(arm_artifacts(&options, mode).rootfs, &root)?;
            stage_fixture(&root, fixture)?;
            let result = execute(&options, &root, mode, sample, position);
            fs::remove_dir_all(&root)?;
            append(&ledger_path, &result?)?;
        }
    }
    let rows = read_ledger(&ledger_path, options.samples)?;
    if rows.len() != options.samples as usize * ORDER.len() {
        return Err("developer benchmark ledger is incomplete".into());
    }
    if let Err(error) = validate_outputs(&rows) {
        fs::write(options.results.join("first-divergence.txt"), format!("{error}\n"))?;
        return Err(error);
    }
    publish_report(&options.results, &rows)
}

fn validate_options(options: &Options) -> Result<(), Error> {
    let translated = arm_artifacts(options, Mode::Translated);
    if !options.rootfs.is_dir()
        || !options.engine.is_file()
        || !translated.rootfs.is_dir()
        || !translated.engine.is_file()
        || !options.native_library.is_file()
    {
        return Err("baseline/translated rootfs, worker, or native library has the wrong type".into());
    }
    Ok(())
}

fn arm_artifacts(options: &Options, mode: Mode) -> ArmArtifacts<'_> {
    if mode == Mode::Translated {
        ArmArtifacts {
            rootfs: options.translated_rootfs.as_deref().unwrap_or(&options.rootfs),
            engine: options.translated_engine.as_deref().unwrap_or(&options.engine),
            guest_isa: options.translated_guest_isa.unwrap_or(options.guest_isa),
        }
    } else {
        ArmArtifacts {
            rootfs: &options.rootfs,
            engine: &options.engine,
            guest_isa: options.guest_isa,
        }
    }
}

const fn host_isa() -> HostIsa {
    if cfg!(target_arch = "aarch64") {
        HostIsa::Aarch64
    } else {
        HostIsa::X86_64
    }
}

const fn execution_backend(host_isa: HostIsa, mode: Mode, guest_isa: GuestIsa) -> ExecutionBackend {
    match mode {
        Mode::Native => ExecutionBackend::HostNative,
        Mode::Supervised => ExecutionBackend::NativeSupervised,
        Mode::Translated => match guest_isa {
            GuestIsa::X86_64 => ExecutionBackend::X86Transliterator,
            GuestIsa::Aarch64 if matches!(host_isa, HostIsa::Aarch64) => ExecutionBackend::Aarch64Transliterator,
            GuestIsa::Aarch64 => ExecutionBackend::Aarch64Interpreter,
        },
    }
}

fn require_identity(recorded: &Identity, expected: &Identity) -> Result<(), Error> {
    let left = serde_json::to_vec(recorded)?;
    let right = serde_json::to_vec(expected)?;
    if left == right {
        Ok(())
    } else {
        Err("resume identity differs from the recorded campaign".into())
    }
}

fn clone_root(source: &Path, destination: &Path) -> Result<(), Error> {
    let status = Command::new("cp")
        .args(["-a", "--reflink=auto"])
        .arg(format!("{}/.", source.display()))
        .arg(destination)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err("rootfs clone failed".into())
    }
}

fn stage_fixture(root: &Path, script: &str) -> Result<(), Error> {
    let work = root.join("work");
    write_workload_tree(&work, script)?;
    let status = Command::new("chroot").arg(root).args(["/bin/sh", "-c", "cd /work && git init -q && git config user.email bench@example.invalid && git config user.name Bench && git add . && git commit -qm seed && git clone -q --bare . /fixture.git"]).status()?;
    if status.success() {
        Ok(())
    } else {
        Err("developer fixture staging failed".into())
    }
}

fn write_workload_tree(work: &Path, script: &str) -> Result<(), Error> {
    fs::create_dir_all(work.join("src"))?;
    fs::create_dir_all(work.join("app"))?;
    for number in 1..=UNIT_COUNT {
        fs::write(work.join(format!("src/unit_{number:03}.c")), unit_source(number))?;
    }
    fs::write(work.join("app/main.c"), main_source())?;
    fs::write(work.join("Makefile"), makefile())?;
    fs::write(work.join("session.sh"), script)?;
    Ok(())
}

fn makefile() -> &'static str {
    "CC ?= gcc\nSRC := $(wildcard src/*.c)\nOBJ := $(SRC:src/%.c=build/%.o)\nall: build/devlib.a build/devcheck\nbuild/%.o: src/%.c\n\t@mkdir -p build\n\t$(CC) -O2 -g -c $< -o $@\nbuild/devlib.a: $(OBJ)\n\tar rcs $@ $^\nbuild/devcheck: app/main.c build/devlib.a\n\t$(CC) -O2 -g $< build/devlib.a -o $@\ntest: all\n\ttest \"$$(ar t build/devlib.a | wc -l)\" -eq 128\n\ttest \"$$(./build/devcheck)\" = \"devcheck:16512\"\n\t./build/devcheck\n"
}

fn unit_source(number: u32) -> String {
    format!("int unit_{number:03}(int x){{return x+{number};}}\n")
}

fn main_source() -> String {
    let mut source = String::from("#include <stdio.h>\n");
    for number in 1..=UNIT_COUNT {
        source.push_str(&format!("int unit_{number:03}(int);\n"));
    }
    source.push_str("int main(void){long total=0;\n");
    for number in 1..=UNIT_COUNT {
        source.push_str(&format!("total += unit_{number:03}({number});\n"));
    }
    source.push_str("printf(\"devcheck:%ld\\n\", total); return total == 16512 ? 0 : 1;}\n");
    source
}

fn fixture_source() -> &'static str {
    r#"#!/bin/sh
set -eu
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
cd /work
mark(){ printf 'HL_PHASE %s\n' "$1" >&2; }
semantic(){ name=$1; hash=$(sha256sum | cut -d' ' -f1); printf 'HL_SEM %s=%s\n' "$name" "$hash"; }
mark prompt; printf 'prompt-ok:%s\n' "$(id -u)"
mark git; rm -rf checkout; git clone -q /fixture.git checkout; cd checkout; git status --porcelain=v1
mark search; rg -n 'unit_(001|064|128)' src | LC_ALL=C sort | semantic search-content; find src -type f -print0 | sort -z | xargs -0 sha256sum | semantic search-files; LC_ALL=C ls -R src | semantic search-tree
mark full-build; make -s -j2 all; semantic full-build-artifact <build/devlib.a
mark edit; printf '\n/* incremental edit */\n' >>src/unit_064.c
mark incremental-build; make -s -j2 all; semantic incremental-build-artifact <build/devlib.a
mark test; make -s test
mark archive; tar -cf package.tar src Makefile; rm -rf extracted; mkdir extracted; tar -xf package.tar -C extracted; find extracted -type f -print0 | sort -z | xargs -0 sha256sum | semantic archive-source
mark package-metadata; apk info -vv >package-metadata.txt; test -s package-metadata.txt; LC_ALL=C sort package-metadata.txt | semantic package-metadata
mark spawn; i=0; while [ "$i" -lt 300 ]; do /bin/sh -c ':'; i=$((i+1)); done
mark final; git diff -- src/unit_064.c | semantic final-diff
printf 'HL_DONE\n' >&2
"#
}

fn workload_recipe_identity() -> String {
    let main = main_source();
    workload_recipe_identity_with(
        UNIT_COUNT,
        unit_source,
        &main,
        makefile(),
        fixture_source(),
        &PHASES,
        &ORDER,
    )
}

fn workload_recipe_identity_with(
    unit_count: u32,
    source: impl Fn(u32) -> String,
    main: &str,
    makefile: &str,
    fixture: &str,
    phases: &[&str],
    order: &[Mode],
) -> String {
    let mut digest = Sha256::new();
    let mut bind = |bytes: &[u8]| {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    bind(&unit_count.to_le_bytes());
    for number in 1..=unit_count {
        bind(source(number).as_bytes());
    }
    bind(main.as_bytes());
    bind(makefile.as_bytes());
    bind(fixture.as_bytes());
    for phase in phases {
        bind(phase.as_bytes());
    }
    for mode in order {
        bind(&[match mode {
            Mode::Native => 0,
            Mode::Supervised => 1,
            Mode::Translated => 2,
        }]);
    }
    hex(digest.finalize())
}

fn execute(options: &Options, root: &Path, mode: Mode, sample: u32, position: usize) -> Result<Row, Error> {
    let guest_isa = arm_artifacts(options, mode).guest_isa;
    let host_isa = host_isa();
    let backend = execution_backend(host_isa, mode, guest_isa);
    let null_counters = measure_null(options, root, mode, sample, position)?;
    let measured = measured_argv(options, root, mode, "bin/sh", &["/work/session.sh"]);
    let perf_path = options
        .results
        .join(format!(".perf-{}-{sample}-{position}", std::process::id()));
    let mut command = perf_command(&perf_path, &measured);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or("child stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("child stderr unavailable")?;
    let marks = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let reader_marks = Arc::clone(&marks);
    let reader_captured = Arc::clone(&captured);
    let reader = std::thread::spawn(move || -> std::io::Result<()> {
        for line in BufReader::new(stderr).lines() {
            let line = line?;
            if let Some(name) = line.strip_prefix("HL_PHASE ") {
                reader_marks
                    .lock()
                    .unwrap()
                    .push((name.to_owned(), start.elapsed().as_nanos()));
            }
            reader_captured
                .lock()
                .unwrap()
                .extend_from_slice(format!("{line}\n").as_bytes());
        }
        Ok(())
    });
    let mut output = Vec::new();
    BufReader::new(stdout).read_to_end(&mut output)?;
    let status = child.wait()?;
    reader.join().map_err(|_| "stderr reader panicked")??;
    let elapsed = start.elapsed().as_nanos();
    if !status.success() {
        return Err(format!("developer session failed in {mode:?}: {status}").into());
    }
    let counters = parse_perf(&fs::read_to_string(&perf_path)?)?;
    fs::remove_file(perf_path)?;
    let marks = Arc::try_unwrap(marks)
        .map_err(|_| "phase reader remained shared")?
        .into_inner()?;
    let stderr = Arc::try_unwrap(captured)
        .map_err(|_| "stderr reader remained shared")?
        .into_inner()?;
    let phase_ns = validate_phases(&marks, elapsed)?;
    let receipt = backend_receipt(backend, guest_isa, &String::from_utf8(stderr)?)?;
    let stdout_sha256 = hex(Sha256::digest(&output));
    atomic_bytes(&output_path(&options.results, sample, position, mode), &output)?;
    Ok(Row {
        sample,
        position,
        mode,
        host_isa,
        guest_isa,
        backend,
        wall_ns: elapsed,
        counters,
        null_counters,
        phase_ns,
        stdout_sha256,
        semantic_sha256: semantic_sha256(&output),
        portable_semantic_sha256: portable_semantic_sha256(&output),
        backend_receipt: receipt,
    })
}

fn measured_argv(options: &Options, root: &Path, mode: Mode, program: &str, arguments: &[&str]) -> Vec<OsString> {
    let mut measured = Vec::<OsString>::new();
    match mode {
        Mode::Native => {
            measured.push("chroot".into());
            measured.push(root.as_os_str().to_owned());
            measured.push(format!("/{program}").into());
            measured.extend(arguments.iter().map(OsString::from));
        }
        Mode::Supervised | Mode::Translated => {
            let artifacts = arm_artifacts(options, mode);
            measured.push(artifacts.engine.as_os_str().to_owned());
            measured.extend(
                ["--loader-receipt", "--report-exit", "--diagnostics", "--native-library"]
                    .into_iter()
                    .map(OsString::from),
            );
            measured.push(options.native_library.as_os_str().to_owned());
            measured.push("--guest-isa".into());
            measured.push(artifacts.guest_isa.as_str().into());
            measured.push("--rootfs".into());
            measured.push(root.as_os_str().to_owned());
            match mode {
                Mode::Supervised => {
                    measured.push("--native-supervised".into());
                }
                Mode::Translated => {
                    measured.extend(["--native-supervised=off".into(), "--translit".into()]);
                }
                Mode::Native => unreachable!(),
            }
            measured.push(program.into());
            measured.extend(arguments.iter().map(OsString::from));
        }
    }
    measured
}

fn perf_command(path: &Path, measured: &[OsString]) -> Command {
    let mut command = Command::new("perf");
    command
        .args(["stat", "-x", "\t", "-o"])
        .arg(path)
        .args(["-e", "duration_time,task-clock,instructions,cycles,page-faults", "--"])
        .args(measured);
    command
}

fn measure_null(options: &Options, root: &Path, mode: Mode, sample: u32, position: usize) -> Result<Counters, Error> {
    let path = options
        .results
        .join(format!(".null-perf-{}-{sample}-{position}", std::process::id()));
    let measured = measured_argv(options, root, mode, "bin/true", &[]);
    let output = perf_command(&path, &measured).output()?;
    if !output.status.success() || !output.stdout.is_empty() {
        return Err(format!("developer null arm failed in {mode:?}: {}", output.status).into());
    }
    let guest_isa = arm_artifacts(options, mode).guest_isa;
    backend_receipt(
        execution_backend(host_isa(), mode, guest_isa),
        guest_isa,
        &String::from_utf8(output.stderr)?,
    )?;
    let counters = parse_perf(&fs::read_to_string(&path)?)?;
    fs::remove_file(path)?;
    Ok(counters)
}

fn parse_perf(text: &str) -> Result<Counters, Error> {
    let value = |event: &str| -> Result<&str, Error> {
        text.lines()
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                fields
                    .iter()
                    .any(|field| field.trim() == event)
                    .then(|| fields[0].trim())
            })
            .ok_or_else(|| format!("perf output omitted {event}").into())
    };
    let counters = Counters {
        duration_ns: value("duration_time")?.parse()?,
        task_clock_ms: value("task-clock")?.parse()?,
        instructions: value("instructions")?.parse()?,
        cycles: value("cycles")?.parse()?,
        page_faults: value("page-faults")?.parse()?,
    };
    if counters.duration_ns == 0
        || !counters.task_clock_ms.is_finite()
        || counters.task_clock_ms <= 0.0
        || counters.instructions == 0
        || counters.cycles == 0
        || counters.page_faults == 0
    {
        return Err("perf output contains an absent or zero counter".into());
    }
    Ok(counters)
}

fn validate_phases(marks: &[(String, u128)], end: u128) -> Result<Vec<(String, u128)>, Error> {
    if marks.len() != PHASES.len()
        || !marks
            .iter()
            .zip(PHASES)
            .all(|((actual, _), expected)| actual == expected)
    {
        return Err("phase sequence differs from the developer protocol".into());
    }
    let mut result = Vec::with_capacity(marks.len());
    for index in 0..marks.len() {
        let next = marks.get(index + 1).map_or(end, |entry| entry.1);
        if next < marks[index].1 {
            return Err("phase clock moved backwards".into());
        }
        result.push((marks[index].0.clone(), next - marks[index].1));
    }
    Ok(result)
}

fn backend_receipt(backend: ExecutionBackend, guest_isa: GuestIsa, stderr: &str) -> Result<String, Error> {
    let evidence: String = match backend {
        ExecutionBackend::HostNative => "host-native".into(),
        ExecutionBackend::NativeSupervised => stderr
            .lines()
            .find(|line| line.starts_with("[hl-native-supervised]") && line.contains("selected=1"))
            .map(str::to_owned)
            .ok_or_else(|| -> Error { "native-supervised arm did not prove selected=1".into() })?,
        ExecutionBackend::X86Transliterator | ExecutionBackend::Aarch64Transliterator => {
            let shape = crate::runtime::backend_shape_product(stderr.as_bytes(), true)?
                .ok_or("translated arm emitted no backend-shape receipt")?;
            validate_backend_shape(backend, &shape)?;
            backend_shape_receipt(stderr).expect("typed parser established one backend-shape record")
        }
        ExecutionBackend::Aarch64Interpreter => {
            let shape = crate::runtime::backend_shape_product(stderr.as_bytes(), true)?
                .ok_or("interpreter arm emitted no backend-shape receipt")?;
            validate_backend_shape(backend, &shape)?;
            backend_shape_receipt(stderr).expect("typed parser established one backend-shape record")
        }
    };
    Ok(format!(
        "guest_isa={} backend={backend:?} {evidence}",
        guest_isa.as_str()
    ))
}

fn validate_backend_shape(
    backend: ExecutionBackend,
    shape: &std::collections::BTreeMap<&str, u64>,
) -> Result<(), Error> {
    match backend {
        ExecutionBackend::X86Transliterator | ExecutionBackend::Aarch64Transliterator
            if shape.get("translation_codegen_available") == Some(&1)
                && shape.get("translated_entries").is_some_and(|value| *value > 0) =>
        {
            Ok(())
        }
        ExecutionBackend::Aarch64Interpreter
            if shape.get("translation_codegen_available") == Some(&0)
                && shape.get("interpreted_entries").is_some_and(|value| *value > 0)
                && shape.get("translated_entries") == Some(&0) =>
        {
            Ok(())
        }
        ExecutionBackend::X86Transliterator | ExecutionBackend::Aarch64Transliterator => {
            Err("translated arm did not prove available codegen and nonzero translated entries".into())
        }
        ExecutionBackend::Aarch64Interpreter => {
            Err("AArch64 interpreter did not prove unavailable codegen and exclusive interpreted execution".into())
        }
        ExecutionBackend::HostNative | ExecutionBackend::NativeSupervised => {
            Err("native backends do not consume a translated backend-shape receipt".into())
        }
    }
}

fn backend_shape_receipt(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find(|line| line.starts_with("[diag] backend-shape "))
        .map(str::to_owned)
}

fn read_ledger(path: &Path, samples: u32) -> Result<Vec<Row>, Error> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    let contents = fs::read(path)?;
    for framed in contents.split_inclusive(|byte| *byte == b'\n') {
        if !framed.ends_with(b"\n") {
            break;
        }
        let row: Row = serde_json::from_slice(&framed[..framed.len() - 1])?;
        if row.sample >= samples
            || row.position >= ORDER.len()
            || row.mode != ORDER[row.position]
            || rows
                .iter()
                .any(|old: &Row| old.sample == row.sample && old.position == row.position)
        {
            return Err("developer ledger contains an invalid or duplicate key".into());
        }
        verify_output(path.parent().ok_or("developer ledger has no result directory")?, &row)?;
        rows.push(row);
    }
    Ok(rows)
}

fn append(path: &Path, row: &Row) -> Result<(), Error> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, row)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn validate_outputs(rows: &[Row]) -> Result<(), Error> {
    let Some(first) = rows.first() else {
        return Err("developer benchmark produced no rows".into());
    };
    if let Some(divergent) = rows.iter().find(|row| {
        (row.guest_isa == first.guest_isa
            && (row.stdout_sha256 != first.stdout_sha256 || row.semantic_sha256 != first.semantic_sha256))
            || row.portable_semantic_sha256 != first.portable_semantic_sha256
    }) {
        return Err(format!(
            "developer benchmark backends produced different output: baseline={:?}/{}/{} stdout={} semantic={}; divergent={:?}/{}/{} stdout={} semantic={}",
            first.mode,
            first.sample,
            first.position,
            first.stdout_sha256,
            first.semantic_sha256,
            divergent.mode,
            divergent.sample,
            divergent.position,
            divergent.stdout_sha256,
            divergent.semantic_sha256,
        )
        .into());
    }
    Ok(())
}

fn publish_report(directory: &Path, rows: &[Row]) -> Result<(), Error> {
    let mut text = String::from(
        "sample\tposition\tmode\thost_isa\tguest_isa\tbackend\twall_ns\tduration_ns\ttask_clock_ms\tinstructions\tcycles\tpage_faults\tnull_duration_ns\tnull_task_clock_ms\tnull_instructions\tnull_cycles\tnull_page_faults\tphase\tphase_ns\n",
    );
    for row in rows {
        for (phase, elapsed) in &row.phase_ns {
            text.push_str(&format!(
                "{}\t{}\t{:?}\t{:?}\t{}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                row.sample,
                row.position,
                row.mode,
                row.host_isa,
                row.guest_isa.as_str(),
                row.backend,
                row.wall_ns,
                row.counters.duration_ns,
                row.counters.task_clock_ms,
                row.counters.instructions,
                row.counters.cycles,
                row.counters.page_faults,
                row.null_counters.duration_ns,
                row.null_counters.task_clock_ms,
                row.null_counters.instructions,
                row.null_counters.cycles,
                row.null_counters.page_faults,
                phase,
                elapsed
            ));
        }
    }
    fs::write(directory.join("report.tsv"), text)?;
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = File::create(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn output_path(directory: &Path, sample: u32, position: usize, mode: Mode) -> PathBuf {
    directory.join(format!("stdout-{sample}-{position}-{mode:?}.txt"))
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(path.parent().ok_or("output artifact has no result directory")?)?.sync_all()?;
    Ok(())
}

fn semantic_sha256(output: &[u8]) -> String {
    let semantic = output
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"HL_SEM "))
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    hex(Sha256::digest(semantic))
}

fn portable_semantic_sha256(output: &[u8]) -> String {
    let mut portable = Vec::with_capacity(output.len());
    for line in output.split_inclusive(|byte| *byte == b'\n') {
        let architecture_bound = [
            b"HL_SEM full-build-artifact=".as_slice(),
            b"HL_SEM incremental-build-artifact=".as_slice(),
        ]
        .into_iter()
        .find(|prefix| line.starts_with(prefix));
        if let Some(prefix) = architecture_bound {
            portable.extend_from_slice(prefix);
            portable.extend_from_slice(b"<architecture-bound-sha256>");
            if line.ends_with(b"\n") {
                portable.push(b'\n');
            }
        } else {
            portable.extend_from_slice(line);
        }
    }
    hex(Sha256::digest(portable))
}

fn verify_output(directory: &Path, row: &Row) -> Result<(), Error> {
    let path = output_path(directory, row.sample, row.position, row.mode);
    let output = fs::read(&path)
        .map_err(|error| format!("cannot reopen completed output artifact {}: {error}", path.display()))?;
    if hex(Sha256::digest(&output)) != row.stdout_sha256
        || semantic_sha256(&output) != row.semantic_sha256
        || portable_semantic_sha256(&output) != row.portable_semantic_sha256
    {
        return Err(format!(
            "completed output artifact {} does not match its ledger row",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cross_isa_options() -> Options {
        Options {
            rootfs: "/roots/x86".into(),
            engine: "/workers/hl-x86_64".into(),
            guest_isa: GuestIsa::X86_64,
            translated_rootfs: Some("/roots/arm".into()),
            translated_engine: Some("/workers/hl-aarch64".into()),
            translated_guest_isa: Some(GuestIsa::Aarch64),
            native_library: "/lib/libhl-native.so".into(),
            results: "/results".into(),
            resume: false,
            samples: 3,
            quiet_seconds: 30,
            lock_timeout: 900,
            max_load: 1.0,
        }
    }

    #[test]
    fn phase_protocol_is_exact_and_individually_timed() {
        let marks = PHASES
            .iter()
            .enumerate()
            .map(|(i, name)| ((*name).to_owned(), i as u128 * 10))
            .collect::<Vec<_>>();
        let phases = validate_phases(&marks, 110).unwrap();
        assert_eq!(phases.len(), 11);
        assert!(phases.iter().all(|(_, elapsed)| *elapsed == 10));
        let mut reordered = marks;
        let left = reordered[3].0.clone();
        reordered[3].0 = reordered[4].0.clone();
        reordered[4].0 = left;
        assert!(validate_phases(&reordered, 110).is_err());
    }

    #[test]
    fn backend_receipts_cannot_be_vacuous() {
        assert_eq!(
            backend_receipt(
                ExecutionBackend::NativeSupervised,
                GuestIsa::X86_64,
                "[hl-native-supervised] selected=1 reason=eligible"
            )
            .unwrap(),
            "guest_isa=x86_64 backend=NativeSupervised [hl-native-supervised] selected=1 reason=eligible"
        );
        assert!(backend_receipt(
            ExecutionBackend::NativeSupervised,
            GuestIsa::X86_64,
            "[hl-native-supervised] selected=0"
        )
        .is_err());
        assert!(backend_receipt(
            ExecutionBackend::X86Transliterator,
            GuestIsa::X86_64,
            "[prof] translit: blocks=9 entries=4"
        )
        .is_err());
        assert!(backend_receipt(
            ExecutionBackend::Aarch64Interpreter,
            GuestIsa::Aarch64,
            "[diag] backend-tree crossings=19 translated_entries=0 interpreted_entries=19"
        )
        .is_err());
        assert!(backend_receipt(
            ExecutionBackend::Aarch64Interpreter,
            GuestIsa::Aarch64,
            "[prof] translit: blocks=9 entries=4"
        )
        .is_err());
    }

    #[test]
    fn translated_receipt_uses_the_product_backend_shape_prefix() {
        let shape = "[diag] backend-shape version=13 crossings=19 translated_entries=0 interpreted_entries=19";
        assert_eq!(backend_shape_receipt(shape).as_deref(), Some(shape));
        assert!(backend_shape_receipt("[diag] backend-tree crossings=19").is_none());
    }

    #[test]
    fn schedule_is_the_balanced_six_arm_order() {
        assert_eq!(
            ORDER,
            [
                Mode::Native,
                Mode::Supervised,
                Mode::Translated,
                Mode::Translated,
                Mode::Supervised,
                Mode::Native
            ]
        );
    }

    #[test]
    fn developer_fixture_closes_over_the_same_guest_path_for_every_backend() {
        let fixture = fixture_source();
        assert!(
            fixture.starts_with(
                "#!/bin/sh\nset -eu\nexport PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n"
            ),
            "the host environment must not decide which guest tools the workload executes"
        );
        assert!(
            fixture.contains(
                "apk info -vv >package-metadata.txt; test -s package-metadata.txt; LC_ALL=C sort package-metadata.txt | semantic package-metadata"
            ),
            "a missing package producer must not be hidden by a successful pipeline tail"
        );
    }

    #[test]
    fn translated_arm_selects_its_own_worker_root_and_typed_guest() {
        let options = cross_isa_options();
        let translated = arm_artifacts(&options, Mode::Translated);
        assert_eq!(translated.rootfs, Path::new("/roots/arm"));
        assert_eq!(translated.engine, Path::new("/workers/hl-aarch64"));
        assert_eq!(translated.guest_isa, GuestIsa::Aarch64);
        let argv = measured_argv(
            &options,
            Path::new("/cloned-arm-root"),
            Mode::Translated,
            "bin/true",
            &[],
        );
        let argv = argv.iter().map(|value| value.to_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(argv[0], "/workers/hl-aarch64");
        assert!(argv.windows(2).any(|pair| pair == ["--guest-isa", "aarch64"]));
        assert!(argv.windows(2).any(|pair| pair == ["--rootfs", "/cloned-arm-root"]));

        let supervised = arm_artifacts(&options, Mode::Supervised);
        assert_eq!(supervised.rootfs, Path::new("/roots/x86"));
        assert_eq!(supervised.engine, Path::new("/workers/hl-x86_64"));
        assert_eq!(supervised.guest_isa, GuestIsa::X86_64);
    }

    #[test]
    fn same_isa_defaults_preserve_the_original_worker_and_root() {
        let mut options = cross_isa_options();
        options.translated_rootfs = None;
        options.translated_engine = None;
        options.translated_guest_isa = None;
        let translated = arm_artifacts(&options, Mode::Translated);
        assert_eq!(translated.rootfs, options.rootfs);
        assert_eq!(translated.engine, options.engine);
        assert_eq!(translated.guest_isa, GuestIsa::X86_64);
    }

    #[test]
    fn workload_recipe_binds_generated_sources_and_protocol() {
        let original = workload_recipe_identity();
        let main = main_source();
        let changed_unit = workload_recipe_identity_with(
            UNIT_COUNT,
            |number| {
                if number == 64 {
                    format!("{}/* changed */\n", unit_source(number))
                } else {
                    unit_source(number)
                }
            },
            &main,
            makefile(),
            fixture_source(),
            &PHASES,
            &ORDER,
        );
        assert_ne!(original, changed_unit);
        let mut changed_order = ORDER;
        changed_order.swap(0, 1);
        assert_ne!(
            original,
            workload_recipe_identity_with(
                UNIT_COUNT,
                unit_source,
                &main,
                makefile(),
                fixture_source(),
                &PHASES,
                &changed_order,
            )
        );
        assert!(main.contains("total += unit_001(1);"));
        assert!(main.contains("total += unit_128(128);"));
        assert_eq!(main.matches("total += unit_").count(), UNIT_COUNT as usize);
        assert!(makefile().contains("test \"$$(./build/devcheck)\" = \"devcheck:16512\""));
    }

    #[test]
    fn generated_workload_links_and_executes_every_unit() {
        let directory = tempfile::tempdir().unwrap();
        write_workload_tree(directory.path(), fixture_source()).unwrap();
        let output = Command::new("make")
            .args(["-s", "test"])
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"devcheck:16512\n");
    }

    #[test]
    fn every_host_guest_backend_is_named_explicitly() {
        assert_eq!(
            execution_backend(HostIsa::X86_64, Mode::Translated, GuestIsa::X86_64),
            ExecutionBackend::X86Transliterator
        );
        assert_eq!(
            execution_backend(HostIsa::Aarch64, Mode::Translated, GuestIsa::X86_64),
            ExecutionBackend::X86Transliterator
        );
        assert_eq!(
            execution_backend(HostIsa::X86_64, Mode::Translated, GuestIsa::Aarch64),
            ExecutionBackend::Aarch64Interpreter
        );
        assert_eq!(
            execution_backend(HostIsa::Aarch64, Mode::Translated, GuestIsa::Aarch64),
            ExecutionBackend::Aarch64Transliterator
        );
        assert_eq!(
            execution_backend(HostIsa::Aarch64, Mode::Native, GuestIsa::Aarch64),
            ExecutionBackend::HostNative
        );
        assert_eq!(
            execution_backend(HostIsa::X86_64, Mode::Supervised, GuestIsa::X86_64),
            ExecutionBackend::NativeSupervised
        );
    }

    #[test]
    fn valid_translated_and_interpreted_shapes_are_accepted() {
        let translated = std::collections::BTreeMap::from([
            ("translation_codegen_available", 1),
            ("translated_entries", 7),
            ("interpreted_entries", 0),
        ]);
        assert!(validate_backend_shape(ExecutionBackend::X86Transliterator, &translated).is_ok());
        assert!(validate_backend_shape(ExecutionBackend::Aarch64Transliterator, &translated).is_ok());
        let interpreted = std::collections::BTreeMap::from([
            ("translation_codegen_available", 0),
            ("translated_entries", 0),
            ("interpreted_entries", 7),
        ]);
        assert!(validate_backend_shape(ExecutionBackend::Aarch64Interpreter, &interpreted).is_ok());
    }

    #[test]
    fn every_backend_must_produce_the_same_semantics() {
        let row = |mode, guest_isa, output: &str, portable: &str| Row {
            sample: 0,
            position: 0,
            mode,
            host_isa: HostIsa::X86_64,
            guest_isa,
            backend: execution_backend(HostIsa::X86_64, mode, guest_isa),
            wall_ns: 1,
            counters: Counters {
                duration_ns: 1,
                task_clock_ms: 1.0,
                instructions: 1,
                cycles: 1,
                page_faults: 1,
            },
            null_counters: Counters {
                duration_ns: 1,
                task_clock_ms: 1.0,
                instructions: 1,
                cycles: 1,
                page_faults: 1,
            },
            phase_ns: Vec::new(),
            stdout_sha256: output.into(),
            semantic_sha256: output.into(),
            portable_semantic_sha256: portable.into(),
            backend_receipt: String::new(),
        };
        assert!(validate_outputs(&[
            row(Mode::Native, GuestIsa::X86_64, "same", "portable"),
            row(
                Mode::Translated,
                GuestIsa::Aarch64,
                "different-architecture",
                "portable"
            ),
        ])
        .is_ok());
        let error = validate_outputs(&[
            row(Mode::Native, GuestIsa::X86_64, "same", "portable"),
            row(Mode::Supervised, GuestIsa::X86_64, "different", "portable"),
        ])
        .unwrap_err()
        .to_string();
        assert!(error.contains("baseline=Native/0/0 stdout=same semantic=same"));
        assert!(error.contains("divergent=Supervised/0/0 stdout=different semantic=different"));
        assert!(validate_outputs(&[
            row(Mode::Native, GuestIsa::X86_64, "same", "portable"),
            row(Mode::Translated, GuestIsa::Aarch64, "different", "wrong-portable"),
        ])
        .is_err());
    }

    #[test]
    fn portable_semantics_ignore_only_architecture_bound_digest_values() {
        let x86 = b"prefix\nHL_SEM search-content=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nHL_SEM full-build-artifact=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
        let arm = b"prefix\nHL_SEM search-content=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nHL_SEM full-build-artifact=fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\n";
        assert_ne!(semantic_sha256(x86), semantic_sha256(arm));
        assert_eq!(portable_semantic_sha256(x86), portable_semantic_sha256(arm));
        let corrupted_search = b"prefix\nHL_SEM search-content=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nHL_SEM full-build-artifact=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n";
        assert_ne!(
            portable_semantic_sha256(x86),
            portable_semantic_sha256(corrupted_search)
        );
    }

    #[test]
    fn completed_rows_require_their_exact_durable_output_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let output = b"abc\n";
        let row = Row {
            sample: 0,
            position: 0,
            mode: Mode::Native,
            host_isa: HostIsa::X86_64,
            guest_isa: GuestIsa::X86_64,
            backend: ExecutionBackend::HostNative,
            wall_ns: 1,
            counters: Counters {
                duration_ns: 1,
                task_clock_ms: 1.0,
                instructions: 1,
                cycles: 1,
                page_faults: 1,
            },
            null_counters: Counters {
                duration_ns: 1,
                task_clock_ms: 1.0,
                instructions: 1,
                cycles: 1,
                page_faults: 1,
            },
            phase_ns: Vec::new(),
            stdout_sha256: hex(Sha256::digest(output)),
            semantic_sha256: semantic_sha256(output),
            portable_semantic_sha256: portable_semantic_sha256(output),
            backend_receipt: "host-native".into(),
        };
        let ledger = directory.path().join("ledger.jsonl");
        append(&ledger, &row).unwrap();
        assert!(read_ledger(&ledger, 3)
            .err()
            .unwrap()
            .to_string()
            .contains("cannot reopen"));

        atomic_bytes(&output_path(directory.path(), 0, 0, Mode::Native), output).unwrap();
        assert_eq!(read_ledger(&ledger, 3).unwrap().len(), 1);
        fs::write(output_path(directory.path(), 0, 0, Mode::Native), b"tampered\n").unwrap();
        assert!(read_ledger(&ledger, 3)
            .err()
            .unwrap()
            .to_string()
            .contains("does not match"));
    }

    #[test]
    fn wall_clock_stops_before_host_evidence_io_and_output_is_durable_before_publication() {
        let source = include_str!("developer.rs");
        let completed = source.find("reader.join().map_err").unwrap();
        let elapsed = source.find("let elapsed = start.elapsed().as_nanos();").unwrap();
        let perf_read = source.find("parse_perf(&fs::read_to_string(&perf_path)?").unwrap();
        assert!(completed < elapsed && elapsed < perf_read);

        let atomic = source.find("fn atomic_bytes(").unwrap();
        let atomic = &source[atomic..source.find("fn semantic_sha256(").unwrap()];
        let write = atomic.find("file.write_all(bytes)?").unwrap();
        let file_sync = atomic.find("file.sync_all()?").unwrap();
        let rename = atomic.find("fs::rename(&temporary, path)?").unwrap();
        let directory_sync = atomic.find("File::open(path.parent()").unwrap();
        assert!(write < file_sync && file_sync < rename && rename < directory_sync);
    }

    #[test]
    fn perf_counters_are_complete_numeric_and_nonzero() {
        let text = "11\t\tduration_time\n2.5\tmsec\ttask-clock\n31\t\tinstructions\n41\t\tcycles\n5\t\tpage-faults\n";
        let parsed = parse_perf(text).unwrap();
        assert_eq!(parsed.instructions, 31);
        assert_eq!(parsed.page_faults, 5);
        assert!(parse_perf(&text.replace("31\t\tinstructions", "0\t\tinstructions")).is_err());
        assert!(parse_perf(&text.replace("41\t\tcycles\n", "")).is_err());
    }

    #[test]
    fn resume_drops_only_a_torn_final_ledger_record() {
        let directory = tempfile::tempdir().unwrap();
        let ledger = directory.path().join("ledger.jsonl");
        fs::write(&ledger, b"{\"sample\":0").unwrap();
        assert!(read_ledger(&ledger, 3).unwrap().is_empty());
        fs::write(&ledger, b"{not-json}\n").unwrap();
        assert!(read_ledger(&ledger, 3).is_err());
    }
}
