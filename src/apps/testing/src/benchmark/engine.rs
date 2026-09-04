//! Measure an already-staged engine, excluding the harness and all preparation.

use super::perf::{Counters, parse as parse_perf};
use crate::suite::Error;
use clap::Args;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Args)]
pub(crate) struct Options {
    /// Exact, already-built production engine executable. Wrappers are deliberately impossible.
    #[arg(long)]
    engine: PathBuf,
    /// Fresh output JSON path.
    #[arg(long)]
    output: PathBuf,
    /// Arguments passed verbatim to the engine, including its guest executable and arguments.
    #[arg(last = true, required = true)]
    arguments: Vec<OsString>,
}

#[derive(Serialize)]
struct Evidence {
    schema: &'static str,
    engine: String,
    engine_sha256: String,
    wall_ns: u128,
    counters: Counters,
    stdout_sha256: String,
    stderr_sha256: String,
    stderr: String,
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    if !options.engine.is_absolute() || !options.engine.is_file() {
        return Err("benchmark engine must be an absolute path to an existing file".into());
    }
    if options.output.exists() {
        return Err("benchmark engine output already exists".into());
    }
    let perf = options.output.with_extension("perf.tmp");
    let mut command = perf_command(&perf, &options.engine, &options.arguments);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let result = command.output()?;
    if !result.status.success() {
        let _ = fs::remove_file(&perf);
        return Err(format!("measured engine failed: {}", result.status).into());
    }
    let counters = parse_perf(&fs::read_to_string(&perf)?)?;
    fs::remove_file(perf)?;
    let evidence = Evidence {
        schema: "husklet-engine-measurement-v1",
        engine: options.engine.display().to_string(),
        engine_sha256: sha256(&fs::read(&options.engine)?),
        wall_ns: u128::from(counters.duration_ns),
        counters,
        stdout_sha256: sha256(&result.stdout),
        stderr_sha256: sha256(&result.stderr),
        stderr: String::from_utf8(result.stderr)?,
    };
    let bytes = serde_json::to_vec(&evidence)?;
    atomic_bytes(&options.output, &bytes)
}

fn perf_command(perf: &std::path::Path, engine: &std::path::Path, arguments: &[OsString]) -> Command {
    let mut command = Command::new("perf");
    command
        .args(["stat", "--inherit", "-x", "\t", "-o"])
        .arg(perf)
        .args(["-e", "duration_time,task-clock,instructions,cycles,page-faults", "--"])
        .arg(engine)
        .args(arguments);
    command
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn atomic_bytes(path: &std::path::Path, bytes: &[u8]) -> Result<(), Error> {
    let parent = path.parent().ok_or("benchmark engine output has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".engine-measurement-{}.tmp", std::process::id()));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perf_boundary_is_exact_engine_and_inherits_its_process_tree() {
        let command = perf_command(
            std::path::Path::new("/settled/hl-aarch64"),
            std::path::Path::new("/settled/hl-aarch64"),
            &["--rootfs".into(), "/settled/root".into(), "bin/work".into()],
        );
        let argv = command
            .get_args()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>();
        let boundary = argv.iter().position(|value| value == "--").unwrap();
        assert_eq!(argv[boundary + 1], "/settled/hl-aarch64");
        assert_eq!(&argv[boundary + 2..], &["--rootfs", "/settled/root", "bin/work"]);
        assert!(argv[..boundary].iter().any(|value| value == "--inherit"));
        assert!(
            !argv
                .iter()
                .any(|value| matches!(value.as_ref(), "testing" | "nix" | "realpath" | "cargo"))
        );
    }
}
