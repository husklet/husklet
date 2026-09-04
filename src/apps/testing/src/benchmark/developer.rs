//! A real edit/build/test session, measured under native and translated execution backends.
//!
//! The checkpoint acceptance plan additionally requires two real capture/restore cycles. They do not
//! belong in this runner until it owns a production checkpoint controller: phase markers alone would be
//! a fake boundary that proves no process state was captured.

use super::{
    evidence::Measurement,
    identity,
    perf::{Counters, parse as parse_perf},
};
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

const SCHEMA: &str = "husklet-developer-benchmark-v9";
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
    /// Immutable description of the physical host or VM (CPU, kernel, hypervisor and allocation).
    #[arg(long)]
    host_manifest: PathBuf,
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
    /// Separate cache-safe campaign; the default cold schedule is unchanged.
    #[arg(long)]
    warm_translation_cache: bool,
    /// Collect translated JCC mechanism counts. Perturbs timing and is not acceptance-comparable.
    #[arg(long)]
    translated_route_observe: bool,
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
    /// AArch64 DBT on x86, with per-instruction interpreter fallback.
    Aarch64DbtWithInterpreterFallback,
    /// AArch64 execution with code generation unavailable.
    Aarch64Interpreter,
}

impl GuestIsa {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }

    const fn target(self) -> crate::suite::Target {
        match self {
            Self::Aarch64 => crate::suite::Target::Arm64,
            Self::X86_64 => crate::suite::Target::Amd64,
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
#[serde(deny_unknown_fields)]
struct Identity {
    schema: String,
    host_manifest: String,
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
    #[serde(default)]
    warm_translation_cache: bool,
    translated_route_observe: bool,
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
    stderr_sha256: String,
    semantic_sha256: String,
    portable_semantic_sha256: String,
    backend_receipt: String,
    translated_route_observe: bool,
}


pub(crate) fn run(options: Options) -> Result<(), Error> {
    validate_options(&options)?;
    let fixture = fixture_source();
    let translated = arm_artifacts(&options, Mode::Translated);
    let expected = Identity {
        schema: SCHEMA.into(),
        host_manifest: identity::artifact_identity(&options.host_manifest)?,
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
        warm_translation_cache: options.warm_translation_cache,
        translated_route_observe: options.translated_route_observe,
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
            let result = (|| {
                if mode == Mode::Native {
                    project_native_hostname(&root)?;
                }
                stage_fixture(&options, &root, mode, fixture)?;
                execute(&options, &root, mode, sample, position)
            })();
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
    if !cfg!(target_os = "linux") || !cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) {
        return Err("developer benchmark requires an AArch64 or x86-64 Linux host".into());
    }
    let perf = Command::new("perf").arg("--version").output();
    if !perf.is_ok_and(|output| output.status.success()) {
        return Err("developer benchmark requires a working perf executable".into());
    }
    let translated = arm_artifacts(options, Mode::Translated);
    if !options.host_manifest.is_file()
        || !options.rootfs.is_dir()
        || !options.engine.is_file()
        || !translated.rootfs.is_dir()
        || !translated.engine.is_file()
        || !options.native_library.is_file()
    {
        return Err(
            "host manifest, baseline/translated rootfs, worker, or native library has the wrong type"
                .into(),
        );
    }
    if options.guest_isa != host_isa().guest() {
        return Err("native and supervised controls require a baseline rootfs matching the host ISA".into());
    }
    verify_artifact_architectures(options)?;
    Ok(())
}

fn verify_artifact_architectures(options: &Options) -> Result<(), Error> {
    use crate::runtime::definition::elf::verify_machine;
    let host = host_isa().guest().target();
    verify_machine(&options.engine, host)?;
    verify_machine(&options.native_library, host)?;
    for mode in [Mode::Native, Mode::Translated] {
        let artifacts = arm_artifacts(options, mode);
        verify_machine(artifacts.engine, host)?;
        for executable in ["bin/sh", "usr/bin/git", "usr/bin/gcc"] {
            let executable = resolve_rootfs_elf(artifacts.rootfs, Path::new(executable))?;
            verify_machine(&executable, artifacts.guest_isa.target())?;
        }
    }
    Ok(())
}

fn resolve_rootfs_elf(root: &Path, relative: &Path) -> Result<PathBuf, Error> {
    use std::{collections::VecDeque, path::Component};
    let mut pending = relative
        .components()
        .map(|part| part.as_os_str().to_owned())
        .collect::<VecDeque<_>>();
    let mut resolved = PathBuf::new();
    let mut followed = 0;
    while let Some(part) = pending.pop_front() {
        match Path::new(&part).components().next() {
            Some(Component::Normal(_)) => resolved.push(&part),
            Some(Component::CurDir) => continue,
            Some(Component::RootDir) => {
                resolved.clear();
                continue;
            }
            Some(Component::ParentDir) if resolved.pop() => continue,
            _ => return Err("rootfs ELF path escapes its root".into()),
        }
        let candidate = root.join(&resolved);
        if fs::symlink_metadata(&candidate)?.file_type().is_symlink() {
            followed += 1;
            if followed > 40 {
                return Err("rootfs ELF path contains a symlink loop".into());
            }
            resolved.pop();
            let target = fs::read_link(candidate)?;
            if target.is_absolute() {
                resolved.clear();
            }
            for component in target.components().rev() {
                pending.push_front(component.as_os_str().to_owned());
            }
        }
    }
    Ok(root.join(resolved))
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

impl HostIsa {
    const fn guest(self) -> GuestIsa {
        match self {
            Self::Aarch64 => GuestIsa::Aarch64,
            Self::X86_64 => GuestIsa::X86_64,
        }
    }
}

const fn execution_backend(host_isa: HostIsa, mode: Mode, guest_isa: GuestIsa) -> ExecutionBackend {
    match mode {
        Mode::Native => ExecutionBackend::HostNative,
        Mode::Supervised => ExecutionBackend::NativeSupervised,
        Mode::Translated => match guest_isa {
            GuestIsa::X86_64 => ExecutionBackend::X86Transliterator,
            GuestIsa::Aarch64 if matches!(host_isa, HostIsa::Aarch64) => ExecutionBackend::Aarch64Transliterator,
            GuestIsa::Aarch64 => ExecutionBackend::Aarch64DbtWithInterpreterFallback,
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

fn project_native_hostname(root: &Path) -> Result<(), Error> {
    let raw = fs::read_to_string("/proc/sys/kernel/hostname")?;
    project_native_hostname_value(root, raw.strip_suffix('\n').unwrap_or(&raw))
}

fn project_native_hostname_value(root: &Path, hostname: &str) -> Result<(), Error> {
    let valid_label = |label: &str| {
        !label.is_empty() && label.len() <= 63 && !label.starts_with('-') && !label.ends_with('-') &&
            label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    };
    if hostname.is_empty() || hostname.len() > 253 || !hostname.split('.').all(valid_label) {
        return Err("host hostname is not a valid DNS name".into());
    }
    let root_meta = fs::symlink_metadata(root)?;
    let etc = root.join("etc");
    let etc_meta = fs::symlink_metadata(&etc)?;
    let hosts = etc.join("hosts");
    let hosts_meta = fs::symlink_metadata(&hosts)?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() || !etc_meta.is_dir() ||
        etc_meta.file_type().is_symlink() || !hosts_meta.is_file() || hosts_meta.file_type().is_symlink()
    {
        return Err("native root /etc/hosts is not a proven regular file beneath the clone".into());
    }
    let canonical_root = fs::canonicalize(root)?;
    let canonical_etc = fs::canonicalize(&etc)?;
    if canonical_etc.parent() != Some(canonical_root.as_path()) {
        return Err("native root /etc escapes the cloned root".into());
    }
    let mut contents = fs::read(&hosts)?;
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        contents.push(b'\n');
    }
    contents.extend_from_slice(format!("127.0.1.1\t{hostname}\n").as_bytes());
    let temporary = etc.join(format!(".hosts.husklet-native-{}", std::process::id()));
    let write_result = (|| {
        let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        output.set_permissions(hosts_meta.permissions())?;
        output.write_all(&contents)?;
        output.sync_all()?;
        fs::rename(&temporary, &hosts)?;
        Ok::<_, Error>(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

const STAGE_COMMAND: &str = "cd /work && git init -q && git config user.email bench@example.invalid && git config user.name Bench && git add . && git commit -qm seed && git clone -q --bare . /fixture.git";

fn stage_argv(options: &Options, root: &Path, mode: Mode) -> Vec<OsString> {
    let artifacts = arm_artifacts(options, mode);
    if artifacts.guest_isa == host_isa().guest() && !(mode == Mode::Translated && options.warm_translation_cache) {
        return [
            "chroot".into(),
            root.as_os_str().to_owned(),
            "/bin/sh".into(),
            "-c".into(),
            STAGE_COMMAND.into(),
        ]
        .into();
    }
    let mut argv: Vec<OsString> = vec![
        artifacts.engine.as_os_str().to_owned(),
        "--report-exit".into(),
        "--native-library".into(),
        options.native_library.as_os_str().to_owned(),
        "--guest-isa".into(),
        artifacts.guest_isa.as_str().into(),
        "--rootfs".into(),
        root.as_os_str().to_owned(),
        "--native-supervised=off".into(),
        "--translit".into(),
        "--".into(),
        "bin/sh".into(),
        "-c".into(),
        STAGE_COMMAND.into(),
    ];
    if mode == Mode::Translated && options.warm_translation_cache {
        let at = argv.iter().position(|value| value == "--").unwrap();
        argv.splice(at..at, ["--translation-cache".into(), options.results.join("translation-cache").into_os_string(),
                            "--translation-cache-observe".into(), "--translation-cache-process-tree".into()]);
    }
    argv
}

fn stage_fixture(options: &Options, root: &Path, mode: Mode, script: &str) -> Result<(), Error> {
    let work = root.join("work");
    if fs::symlink_metadata(&work).is_ok() || fs::symlink_metadata(root.join("fixture.git")).is_ok() {
        return Err("developer rootfs already contains a benchmark-owned staging path".into());
    }
    write_workload_tree(&work, script)?;
    let argv = stage_argv(options, root, mode);
    let status = Command::new(&argv[0]).args(&argv[1..]).status()?;
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
mark search
rg -n 'unit_(001|064|128)' src >search-content.unsorted
LC_ALL=C sort search-content.unsorted | semantic search-content
find src -type f -print0 >search-files.unsorted
sort -z search-files.unsorted >search-files.sorted
xargs -0 sha256sum <search-files.sorted >search-files.hashes
test -s search-files.hashes
semantic search-files <search-files.hashes
LC_ALL=C ls -R src | semantic search-tree
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
    let stderr = String::from_utf8(stderr)?;
    let receipt = backend_receipt(backend, guest_isa, &stderr, &measured, root,
                                  options.translated_route_observe && mode == Mode::Translated)?;
    let stdout_sha256 = hex(Sha256::digest(&output));
    let stderr_sha256 = hex(Sha256::digest(stderr.as_bytes()));
    atomic_bytes(&output_path(&options.results, sample, position, mode), &output)?;
    atomic_bytes(&stderr_path(&options.results, sample, position, mode), stderr.as_bytes())?;
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
        stderr_sha256,
        semantic_sha256: semantic_sha256(&output),
        portable_semantic_sha256: portable_semantic_sha256(&output),
        backend_receipt: receipt,
        translated_route_observe: options.translated_route_observe,
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
            measured.extend(["--loader-receipt", "--report-exit"].into_iter().map(OsString::from));
            if mode == Mode::Translated && options.warm_translation_cache {
                measured.extend(["--translation-cache".into(), options.results.join("translation-cache").into_os_string(),
                                 "--translation-cache-observe".into(), "--translation-cache-process-tree".into()]);
            } else {
                measured.push("--diagnostics".into());
            }
            measured.push("--native-library".into());
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
                    if options.translated_route_observe && !options.warm_translation_cache {
                        measured.push("--translation-cache-observe".into());
                    }
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
        &measured,
        root,
        options.translated_route_observe && mode == Mode::Translated,
    )?;
    let counters = parse_perf(&fs::read_to_string(&path)?)?;
    fs::remove_file(path)?;
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

fn backend_receipt(
    backend: ExecutionBackend,
    guest_isa: GuestIsa,
    stderr: &str,
    measured: &[OsString],
    root: &Path,
    require_jcc_route: bool,
) -> Result<String, Error> {
    let evidence: String = match backend {
        ExecutionBackend::HostNative => {
            if measured.first().and_then(|value| value.to_str()) != Some("chroot")
                || measured.get(1).map(OsString::as_os_str) != Some(root.as_os_str())
            {
                return Err("host-native arm did not execute chroot against its cloned root".into());
            }
            let mut argv = Sha256::new();
            for value in measured {
                let bytes = value.as_encoded_bytes();
                argv.update((bytes.len() as u64).to_le_bytes());
                argv.update(bytes);
            }
            let root_path_sha256 = hex(Sha256::digest(root.as_os_str().as_encoded_bytes()));
            format!("host-native argv_sha256={} root_path_sha256={root_path_sha256}", hex(argv.finalize()))
        }
        ExecutionBackend::NativeSupervised => stderr
            .lines()
            .find(|line| {
                line.starts_with("[hl-native-supervised]")
                    && line.split_ascii_whitespace().any(|field| field == "selected=1")
                    && line.split_ascii_whitespace().any(|field| field == "translated_abi=0")
            })
            .map(str::to_owned)
            .ok_or_else(|| -> Error {
                "native-supervised arm did not prove selected=1 with translated_abi=0".into()
            })?,
        ExecutionBackend::X86Transliterator
        | ExecutionBackend::Aarch64Transliterator
        | ExecutionBackend::Aarch64DbtWithInterpreterFallback => {
            if measured.iter().any(|value| value == "--translation-cache") {
                let hit = stderr.lines().find(|line| line.starts_with("[pcache] exec HIT"))
                    .ok_or("warm translated arm did not prove a persistent-cache hit")?;
                return Ok(format!("guest_isa={} backend={backend:?} cache=warm {hit}", guest_isa.as_str()));
            }
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
    let route = if require_jcc_route {
        format!(" {}", x86_jcc_route_receipt(stderr)?)
    } else { String::new() };
    Ok(format!(
        "guest_isa={} backend={backend:?} {evidence}{route}",
        guest_isa.as_str()
    ))
}

fn x86_jcc_route_receipt(stderr: &str) -> Result<String, Error> {
    let records = stderr.lines()
        .filter(|line| line.starts_with("[diag] x86-jcc-route version=1 "))
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(format!("translated route observation requires exactly one x86-jcc-route v1 record; observed {}", records.len()).into());
    }
    let mut fields = std::collections::BTreeMap::new();
    for field in records[0].split_ascii_whitespace().skip(3) {
        let (name, value) = field.split_once('=').ok_or("malformed x86-jcc-route field")?;
        let value = value.parse::<u64>()
            .map_err(|_| format!("x86-jcc-route field {name} is not an unsigned integer"))?;
        if fields.insert(name, value).is_some() {
            return Err("x86-jcc-route record contains a duplicate field".into());
        }
    }
    for name in ["attempts", "hit", "empty", "collision", "state_refusal", "irq",
                 "known_taken", "known_fallthrough"] {
        if !fields.contains_key(name) {
            return Err(format!("x86-jcc-route record omits {name}").into());
        }
    }
    if fields.len() != 8 {
        return Err("x86-jcc-route record contains an unknown field".into());
    }
    let classified = ["hit", "empty", "collision", "state_refusal", "irq"]
        .into_iter().try_fold(0_u64, |sum, name| sum.checked_add(fields[name]))
        .ok_or("x86-jcc-route classified-count sum overflowed")?;
    if fields["attempts"] != classified {
        return Err(format!("x86-jcc-route does not conserve attempts: {} != {classified}", fields["attempts"]).into());
    }
    Ok(records[0].to_owned())
}

fn validate_backend_shape(
    backend: ExecutionBackend,
    shape: &std::collections::BTreeMap<&str, u64>,
) -> Result<(), Error> {
    match backend {
        ExecutionBackend::X86Transliterator
        | ExecutionBackend::Aarch64Transliterator
        | ExecutionBackend::Aarch64DbtWithInterpreterFallback
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
        ExecutionBackend::X86Transliterator
        | ExecutionBackend::Aarch64Transliterator
        | ExecutionBackend::Aarch64DbtWithInterpreterFallback => {
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
    let mut text = if rows.iter().any(|row| row.translated_route_observe) {
        String::from("# timing=perturbed mechanism-count-mode; not acceptance-comparable\n")
    } else { String::new() };
    text.push_str(
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

fn stderr_path(directory: &Path, sample: u32, position: usize, mode: Mode) -> PathBuf {
    directory.join(format!("stderr-{sample}-{position}-{mode:?}.txt"))
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
    let path = stderr_path(directory, row.sample, row.position, row.mode);
    let stderr = fs::read(&path)
        .map_err(|error| format!("cannot reopen completed stderr artifact {}: {error}", path.display()))?;
    if hex(Sha256::digest(stderr)) != row.stderr_sha256 {
        return Err(format!("completed stderr artifact {} does not match its ledger row", path.display()).into());
    }
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn cross_isa_options() -> Options {
        Options {
            host_manifest: "/identity/host.json".into(),
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
            warm_translation_cache: false,
            translated_route_observe: false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn rootfs_elf_resolution_keeps_absolute_links_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::create_dir_all(root.path().join("usr/bin")).unwrap();
        fs::write(root.path().join("usr/bin/tool"), b"guest").unwrap();
        symlink("/usr/bin/tool", root.path().join("bin/tool")).unwrap();
        assert_eq!(
            resolve_rootfs_elf(root.path(), Path::new("bin/tool")).unwrap(),
            root.path().join("usr/bin/tool")
        );
        symlink("../../outside", root.path().join("bin/escape")).unwrap();
        assert!(resolve_rootfs_elf(root.path(), Path::new("bin/escape")).is_err());
    }

    #[test]
    fn legacy_identity_cannot_resume_as_v9() {
        let error = serde_json::from_str::<Identity>(r#"{"schema":"husklet-developer-benchmark-v3"}"#)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("host_manifest"), "{error}");
        assert_eq!(SCHEMA, "husklet-developer-benchmark-v9");
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
        let receipt = |backend, guest, stderr| backend_receipt(backend, guest, stderr, &[], Path::new("/root"), false);
        assert_eq!(
            receipt(
                ExecutionBackend::NativeSupervised,
                GuestIsa::X86_64,
                "[hl-native-supervised] selected=1 translated_abi=0 reason=eligible"
            )
            .unwrap(),
            "guest_isa=x86_64 backend=NativeSupervised [hl-native-supervised] selected=1 translated_abi=0 reason=eligible"
        );
        assert!(receipt(
            ExecutionBackend::NativeSupervised,
            GuestIsa::X86_64,
            "[hl-native-supervised] selected=1 translated_abi=1"
        )
        .is_err());
        assert!(receipt(
            ExecutionBackend::NativeSupervised,
            GuestIsa::X86_64,
            "[hl-native-supervised] selected=0"
        )
        .is_err());
        assert!(receipt(
            ExecutionBackend::X86Transliterator,
            GuestIsa::X86_64,
            "[prof] translit: blocks=9 entries=4"
        )
        .is_err());
        assert!(receipt(
            ExecutionBackend::Aarch64Interpreter,
            GuestIsa::Aarch64,
            "[diag] backend-tree crossings=19 translated_entries=0 interpreted_entries=19"
        )
        .is_err());
        assert!(receipt(
            ExecutionBackend::Aarch64Interpreter,
            GuestIsa::Aarch64,
            "[prof] translit: blocks=9 entries=4"
        )
        .is_err());
    }

    #[test]
    fn host_native_receipt_binds_the_exact_chroot_argv_and_root() {
        let root = Path::new("/results/root-2-5");
        let argv = ["chroot".into(), root.as_os_str().to_owned(), "/bin/sh".into()];
        let receipt = backend_receipt(ExecutionBackend::HostNative, GuestIsa::X86_64, "", &argv, root, false).unwrap();
        assert!(receipt.contains("backend=HostNative host-native argv_sha256="), "{receipt}");
        assert!(receipt.contains(" root_path_sha256="), "{receipt}");
        let wrong = ["env".into(), root.as_os_str().to_owned(), "/bin/sh".into()];
        assert!(backend_receipt(ExecutionBackend::HostNative, GuestIsa::X86_64, "", &wrong, root, false).is_err());
        assert!(backend_receipt(ExecutionBackend::HostNative, GuestIsa::X86_64, "", &argv,
                                Path::new("/results/other-root"), false).is_err());
    }

    #[test]
    fn native_supervised_producer_refuses_a_translated_abi_before_its_receipt() {
        let source = include_str!(
            "../../../../runtime/hl-native/src/native/engine/native_supervised.c"
        );
        let run = source
            .split_once("static int32_t hl_native_supervised_run(")
            .and_then(|(_, tail)| tail.split_once("static int32_t hl_native_supervised_run("))
            .map_or_else(
                || source.split_once("static int32_t hl_native_supervised_run(").map(|(_, tail)| tail),
                |(production, _)| Some(production),
            )
            .expect("native-supervised production entry");
        let guard = run.find("if (box != NULL) return 70;").expect("translated ABI guard");
        let receipt = run.find("selected=1 translated_abi=%d").expect("selection receipt");
        assert!(guard < receipt, "translated ABI must be refused before selection is reported");
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
    fn warm_cache_is_opt_in_and_does_not_change_the_cold_schedule() {
        let cold = cross_isa_options();
        let mut warm = cross_isa_options();
        warm.warm_translation_cache = true;
        let cold = measured_argv(&cold, Path::new("/root"), Mode::Translated, "bin/true", &[]);
        let warm = measured_argv(&warm, Path::new("/root"), Mode::Translated, "bin/true", &[]);
        assert!(cold.iter().any(|value| value == "--diagnostics"));
        assert!(!cold.iter().any(|value| value == "--translation-cache"));
        assert!(!warm.iter().any(|value| value == "--diagnostics"));
        assert!(warm.iter().any(|value| value == "--translation-cache-process-tree"));
        assert_eq!(ORDER.len(), 6);
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
        assert!(
            fixture.contains("find src -type f -print0 >search-files.unsorted\n")
                && fixture.contains("xargs -0 sha256sum <search-files.sorted >search-files.hashes\n")
                && fixture.contains("test -s search-files.hashes\n")
                && fixture.contains("semantic search-files <search-files.hashes\n"),
            "file discovery and hashing must finish successfully before the semantic consumer runs"
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
        assert!(!argv.contains(&"--translation-cache-observe"));

        let supervised = arm_artifacts(&options, Mode::Supervised);
        assert_eq!(supervised.rootfs, Path::new("/roots/x86"));
        assert_eq!(supervised.engine, Path::new("/workers/hl-x86_64"));
        assert_eq!(supervised.guest_isa, GuestIsa::X86_64);
    }

    #[test]
    fn translated_route_observation_changes_only_the_translated_argv() {
        let mut options = cross_isa_options();
        options.translated_route_observe = true;
        for mode in [Mode::Native, Mode::Supervised, Mode::Translated] {
            let argv = measured_argv(&options, Path::new("/root"), mode, "bin/true", &[]);
            let count = argv.iter().filter(|value| *value == "--translation-cache-observe").count();
            assert_eq!(count, usize::from(mode == Mode::Translated), "{mode:?}");
        }
        assert!(!stage_argv(&options, Path::new("/root"), Mode::Translated)
            .contains(&OsString::from("--translation-cache-observe")));
        options.warm_translation_cache = true;
        let argv = measured_argv(&options, Path::new("/root"), Mode::Translated, "bin/true", &[]);
        assert_eq!(
            argv.iter().filter(|value| *value == "--translation-cache-observe").count(),
            1
        );
    }

    #[test]
    fn translated_route_receipt_is_unique_complete_and_conserved() {
        let valid = "[diag] x86-jcc-route version=1 attempts=6 hit=1 empty=2 collision=1 state_refusal=1 irq=1 known_taken=7 known_fallthrough=9";
        assert_eq!(x86_jcc_route_receipt(valid).unwrap(), valid);
        assert!(x86_jcc_route_receipt("").is_err());
        assert!(x86_jcc_route_receipt(&format!("{valid}\n{valid}")).is_err());
        assert!(x86_jcc_route_receipt("[diag] x86-jcc-route version=1 attempts=5 hit=1 empty=2 collision=1 state_refusal=1 irq=1 known_taken=7 known_fallthrough=9").is_err());
        assert!(x86_jcc_route_receipt("[diag] x86-jcc-route version=1 attempts=6 hit=1 empty=2 collision=1 state_refusal=1 irq=1 known_taken=7").is_err());
        assert!(x86_jcc_route_receipt("[diag] x86-jcc-route version=1 attempts=6 hit=1 empty=2 collision=1 state_refusal=1 irq=1 known_taken=7 known_fallthrough=9 extra=0").is_err());
    }

    #[test]
    fn foreign_fixture_staging_runs_through_the_selected_guest_backend() {
        let options = cross_isa_options();
        let argv = stage_argv(&options, Path::new("/cloned-arm-root"), Mode::Translated);
        let argv = argv.iter().map(|value| value.to_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(argv[0], "/workers/hl-aarch64");
        assert!(argv.windows(2).any(|pair| pair == ["--guest-isa", "aarch64"]));
        assert!(argv.windows(2).any(|pair| pair == ["--rootfs", "/cloned-arm-root"]));
        assert!(argv.contains(&"--native-supervised=off"));
        assert_eq!(argv.last(), Some(&STAGE_COMMAND));
    }

    #[test]
    fn native_control_is_defined_only_for_the_host_isa() {
        assert_eq!(HostIsa::Aarch64.guest(), GuestIsa::Aarch64);
        assert_eq!(HostIsa::X86_64.guest(), GuestIsa::X86_64);
        let mut options = cross_isa_options();
        options.guest_isa = host_isa().guest();
        let argv = stage_argv(&options, Path::new("/cloned-x86-root"), Mode::Native);
        assert_eq!(argv[0], "chroot");
    }

    #[test]
    fn native_hostname_is_projected_only_into_the_cloned_root() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("etc")).unwrap();
        fs::write(source.path().join("etc/hosts"), b"127.0.0.1\tlocalhost\n").unwrap();
        let clone = tempfile::tempdir().unwrap();
        clone_root(source.path(), clone.path()).unwrap();
        project_native_hostname_value(clone.path(), "naa0245").unwrap();
        assert_eq!(fs::read(source.path().join("etc/hosts")).unwrap(), b"127.0.0.1\tlocalhost\n");
        assert_eq!(fs::read(clone.path().join("etc/hosts")).unwrap(),
                   b"127.0.0.1\tlocalhost\n127.0.1.1\tnaa0245\n");
        let production = include_str!("developer.rs").split_once("#[cfg(test)]\nmod tests").unwrap().0;
        assert!(production.find("if mode == Mode::Native {").unwrap() <
                production.find("stage_fixture(&options, &root, mode, fixture)?;").unwrap());
        assert_eq!(production.matches("project_native_hostname(&root)?;").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn native_hostname_projection_refuses_symlinked_etc_or_hosts() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("hosts"), b"outside\n").unwrap();
        let linked_etc = tempfile::tempdir().unwrap();
        symlink(outside.path(), linked_etc.path().join("etc")).unwrap();
        assert!(project_native_hostname_value(linked_etc.path(), "host-a").is_err());
        let linked_hosts = tempfile::tempdir().unwrap();
        fs::create_dir(linked_hosts.path().join("etc")).unwrap();
        symlink(outside.path().join("hosts"), linked_hosts.path().join("etc/hosts")).unwrap();
        assert!(project_native_hostname_value(linked_hosts.path(), "host-a").is_err());
        assert_eq!(fs::read(outside.path().join("hosts")).unwrap(), b"outside\n");
    }

    #[test]
    fn native_hostname_projection_rejects_unvalidated_names() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("etc")).unwrap();
        fs::write(root.path().join("etc/hosts"), b"localhost\n").unwrap();
        for hostname in ["", "bad host", "-edge", "edge-", "host\nspoof"] {
            assert!(project_native_hostname_value(root.path(), hostname).is_err(), "{hostname:?}");
        }
        assert_eq!(fs::read(root.path().join("etc/hosts")).unwrap(), b"localhost\n");
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
            ExecutionBackend::Aarch64DbtWithInterpreterFallback
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
        let mut hybrid = translated.clone();
        hybrid.insert("interpreted_entries", 3);
        assert!(validate_backend_shape(ExecutionBackend::Aarch64DbtWithInterpreterFallback, &hybrid).is_ok());
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
            stderr_sha256: String::new(),
            semantic_sha256: output.into(),
            portable_semantic_sha256: portable.into(),
            backend_receipt: String::new(),
            translated_route_observe: false,
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
        let stderr = b"diagnostic\n";
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
            stderr_sha256: hex(Sha256::digest(stderr)),
            semantic_sha256: semantic_sha256(output),
            portable_semantic_sha256: portable_semantic_sha256(output),
            backend_receipt: "host-native".into(),
            translated_route_observe: false,
        };
        let ledger = directory.path().join("ledger.jsonl");
        append(&ledger, &row).unwrap();
        assert!(read_ledger(&ledger, 3)
            .err()
            .unwrap()
            .to_string()
            .contains("cannot reopen"));

        atomic_bytes(&output_path(directory.path(), 0, 0, Mode::Native), output).unwrap();
        assert!(read_ledger(&ledger, 3)
            .err()
            .unwrap()
            .to_string()
            .contains("stderr artifact"));
        atomic_bytes(&stderr_path(directory.path(), 0, 0, Mode::Native), stderr).unwrap();
        assert_eq!(read_ledger(&ledger, 3).unwrap().len(), 1);
        fs::write(output_path(directory.path(), 0, 0, Mode::Native), b"tampered\n").unwrap();
        assert!(read_ledger(&ledger, 3)
            .err()
            .unwrap()
            .to_string()
            .contains("does not match"));
        atomic_bytes(&output_path(directory.path(), 0, 0, Mode::Native), output).unwrap();
        fs::write(stderr_path(directory.path(), 0, 0, Mode::Native), b"tampered\n").unwrap();
        assert!(read_ledger(&ledger, 3)
            .err()
            .unwrap()
            .to_string()
            .contains("stderr artifact"));
    }

    #[test]
    fn wall_clock_stops_before_host_evidence_io_and_output_is_durable_before_publication() {
        let source = include_str!("developer.rs");
        let production = &source[..source.find("\n#[cfg(test)]\nmod tests").unwrap()];
        let completed = production.find("reader.join().map_err").unwrap();
        let elapsed = production.find("let elapsed = start.elapsed().as_nanos();").unwrap();
        let perf_read = production.find("parse_perf(&fs::read_to_string(&perf_path)?").unwrap();
        assert!(completed < elapsed && elapsed < perf_read);

        let stderr_write = production
            .find("atomic_bytes(&stderr_path(&options.results, sample, position, mode), stderr.as_bytes())?")
            .unwrap();
        let row_return = production[stderr_write..]
            .find("Ok(Row {")
            .map(|offset| stderr_write + offset)
            .unwrap();
        assert!(stderr_write < row_return);
        assert!(production.contains("append(&ledger_path, &result?)?"));

        let atomic = production.find("fn atomic_bytes(").unwrap();
        let atomic = &production[atomic..production.find("fn semantic_sha256(").unwrap()];
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
