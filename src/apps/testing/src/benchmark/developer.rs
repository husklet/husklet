//! A real edit/build/test session, measured under all same-ISA execution backends.

use super::{evidence::Measurement, identity};
use crate::suite::Error;
use clap::Args;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Instant,
};

const SCHEMA: &str = "husklet-developer-benchmark-v1";
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
    /// Immutable, prepared Linux rootfs containing sh, gcc, make, git, rg, tar, and apk metadata.
    #[arg(long)]
    rootfs: PathBuf,
    /// Production hl-x86_64 runner.
    #[arg(long)]
    engine: PathBuf,
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

#[derive(Deserialize, Serialize)]
struct Identity {
    schema: String,
    rootfs: String,
    engine: String,
    native_library: String,
    fixture: String,
    samples: u32,
}

#[derive(Deserialize, Serialize)]
struct Row {
    sample: u32,
    position: usize,
    mode: Mode,
    wall_ns: u128,
    phase_ns: Vec<(String, u128)>,
    stdout_sha256: String,
    semantic_sha256: String,
    backend_receipt: String,
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    validate_options(&options)?;
    let fixture = fixture_source();
    let expected = Identity {
        schema: SCHEMA.into(),
        rootfs: identity::artifact_identity(&options.rootfs)?,
        engine: identity::artifact_identity(&options.engine)?,
        native_library: identity::artifact_identity(&options.native_library)?,
        fixture: hex(Sha256::digest(fixture.as_bytes())),
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
    let _measurement = Measurement::acquire(options.quiet_seconds, options.lock_timeout, options.max_load)?;
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
            clone_root(&options.rootfs, &root)?;
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
    validate_outputs(&rows)?;
    publish_report(&options.results, &rows)
}

fn validate_options(options: &Options) -> Result<(), Error> {
    if !options.rootfs.is_dir() || !options.engine.is_file() || !options.native_library.is_file() {
        return Err("rootfs, engine, or native library has the wrong type".into());
    }
    Ok(())
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
    fs::create_dir_all(work.join("src"))?;
    for number in 1..=128 {
        fs::write(
            work.join(format!("src/unit_{number:03}.c")),
            format!("int unit_{number:03}(int x){{return x+{number};}}\n"),
        )?;
    }
    fs::write(work.join("Makefile"), makefile())?;
    fs::write(work.join("session.sh"), script)?;
    let status = Command::new("chroot").arg(root).args(["/bin/sh", "-c", "cd /work && git init -q && git config user.email bench@example.invalid && git config user.name Bench && git add . && git commit -qm seed && git clone -q --bare . /fixture.git"]).status()?;
    if status.success() {
        Ok(())
    } else {
        Err("developer fixture staging failed".into())
    }
}

fn makefile() -> &'static str {
    "CC ?= gcc\nSRC := $(wildcard src/*.c)\nOBJ := $(SRC:src/%.c=build/%.o)\nall: build/devlib.a\nbuild/%.o: src/%.c\n\t@mkdir -p build\n\t$(CC) -O2 -g -c $< -o $@\nbuild/devlib.a: $(OBJ)\n\tar rcs $@ $^\ntest: all\n\ttest \"$$(ar t build/devlib.a | wc -l)\" -eq 128\n"
}

fn fixture_source() -> &'static str {
    r#"#!/bin/sh
set -eu
cd /work
mark(){ printf 'HL_PHASE %s\n' "$1" >&2; }
mark prompt; printf 'prompt-ok:%s\n' "$(id -u)"
mark git; rm -rf checkout; git clone -q /fixture.git checkout; cd checkout; git status --porcelain=v1
mark search; rg -n 'unit_(001|064|128)' src | LC_ALL=C sort | sha256sum; find src -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum; LC_ALL=C ls -R src | sha256sum
mark full-build; make -s -j2 all; sha256sum build/devlib.a
mark edit; printf '\n/* incremental edit */\n' >>src/unit_064.c
mark incremental-build; make -s -j2 all; sha256sum build/devlib.a
mark test; make -s test
mark archive; tar -cf package.tar src Makefile; rm -rf extracted; mkdir extracted; tar -xf package.tar -C extracted; find extracted -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum
mark package-metadata; apk info -vv | LC_ALL=C sort | sha256sum
mark spawn; i=0; while [ "$i" -lt 300 ]; do /bin/sh -c ':'; i=$((i+1)); done
mark final; git diff -- src/unit_064.c | sha256sum
printf 'HL_DONE\n' >&2
"#
}

fn execute(options: &Options, root: &Path, mode: Mode, sample: u32, position: usize) -> Result<Row, Error> {
    let mut command = match mode {
        Mode::Native => {
            let mut c = Command::new("chroot");
            c.arg(root).args(["/bin/sh", "/work/session.sh"]);
            c
        }
        Mode::Supervised | Mode::Translated => {
            let mut c = Command::new(&options.engine);
            c.args(["--loader-receipt", "--report-exit", "--diagnostics", "--native-library"])
                .arg(&options.native_library)
                .arg("--rootfs")
                .arg(root);
            match mode {
                Mode::Supervised => {
                    c.arg("--native-supervised");
                }
                Mode::Translated => {
                    c.args(["--native-supervised=off", "--translit"]);
                }
                Mode::Native => unreachable!(),
            }
            c.args(["bin/sh", "/work/session.sh"]);
            c
        }
    };
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
    if !status.success() {
        return Err(format!("developer session failed in {mode:?}: {status}").into());
    }
    let elapsed = start.elapsed().as_nanos();
    let marks = Arc::try_unwrap(marks)
        .map_err(|_| "phase reader remained shared")?
        .into_inner()?;
    let stderr = Arc::try_unwrap(captured)
        .map_err(|_| "stderr reader remained shared")?
        .into_inner()?;
    let phase_ns = validate_phases(&marks, elapsed)?;
    let receipt = backend_receipt(mode, &String::from_utf8(stderr)?)?;
    let stdout_sha256 = hex(Sha256::digest(&output));
    let semantic = output
        .split(|byte| *byte == b'\n')
        .filter(|line| line.len() == 67 && line[64..] == *b"  -")
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    Ok(Row {
        sample,
        position,
        mode,
        wall_ns: elapsed,
        phase_ns,
        stdout_sha256,
        semantic_sha256: hex(Sha256::digest(semantic)),
        backend_receipt: receipt,
    })
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

fn backend_receipt(mode: Mode, stderr: &str) -> Result<String, Error> {
    match mode {
        Mode::Native => Ok("host-native".into()),
        Mode::Supervised => stderr
            .lines()
            .find(|line| line.starts_with("[hl-native-supervised]") && line.contains("selected=1"))
            .map(str::to_owned)
            .ok_or_else(|| "native-supervised arm did not prove selected=1".into()),
        Mode::Translated => stderr
            .lines()
            .find(|line| {
                line.starts_with("[prof] translit:")
                    && positive_field(line, "blocks")
                    && positive_field(line, "entries")
            })
            .map(str::to_owned)
            .ok_or_else(|| "translated arm did not prove nonzero blocks and entries".into()),
    }
}

fn positive_field(line: &str, name: &str) -> bool {
    line.split_ascii_whitespace()
        .find_map(|field| {
            field
                .strip_prefix(&format!("{name}="))?
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse::<u64>()
                .ok()
        })
        .is_some_and(|value| value > 0)
}

fn read_ledger(path: &Path, samples: u32) -> Result<Vec<Row>, Error> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let row: Row = serde_json::from_str(&line?)?;
        if row.sample >= samples
            || row.position >= ORDER.len()
            || row.mode != ORDER[row.position]
            || rows
                .iter()
                .any(|old: &Row| old.sample == row.sample && old.position == row.position)
        {
            return Err("developer ledger contains an invalid or duplicate key".into());
        }
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
    if rows
        .iter()
        .any(|row| row.stdout_sha256 != first.stdout_sha256 || row.semantic_sha256 != first.semantic_sha256)
    {
        return Err("developer benchmark backends produced different output".into());
    }
    Ok(())
}

fn publish_report(directory: &Path, rows: &[Row]) -> Result<(), Error> {
    let mut text = String::from("sample\tposition\tmode\twall_ns\tphase\tphase_ns\n");
    for row in rows {
        for (phase, elapsed) in &row.phase_ns {
            text.push_str(&format!(
                "{}\t{}\t{:?}\t{}\t{}\t{}\n",
                row.sample, row.position, row.mode, row.wall_ns, phase, elapsed
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

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        reordered.swap(3, 4);
        assert!(validate_phases(&reordered, 110).is_err());
    }

    #[test]
    fn backend_receipts_cannot_be_vacuous() {
        assert!(backend_receipt(Mode::Supervised, "[hl-native-supervised] selected=1 reason=eligible").is_ok());
        assert!(backend_receipt(Mode::Supervised, "[hl-native-supervised] selected=0").is_err());
        assert!(backend_receipt(Mode::Translated, "[prof] translit: blocks=9 entries=4").is_ok());
        assert!(backend_receipt(Mode::Translated, "[prof] translit: blocks=9 entries=0").is_err());
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
    fn every_backend_must_produce_the_same_semantics() {
        let row = |mode, output: &str| Row {
            sample: 0,
            position: 0,
            mode,
            wall_ns: 1,
            phase_ns: Vec::new(),
            stdout_sha256: output.into(),
            semantic_sha256: output.into(),
            backend_receipt: String::new(),
        };
        assert!(validate_outputs(&[row(Mode::Native, "same"), row(Mode::Translated, "same")]).is_ok());
        assert!(validate_outputs(&[row(Mode::Native, "same"), row(Mode::Supervised, "different")]).is_err());
    }
}
