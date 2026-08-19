//! Near-native performance floor: fixed per-process cost, per-syscall crossing cost,
//! and a guest-CPU control, each measured against a bare-host baseline arm.
//!
//! Every design decision here exists because an ad-hoc harness got the answer wrong:
//!
//! * **Nothing is attributed across `fork`.** The only timer is wall clock taken in the
//!   guest driver's parent around a `fork`/`execve`/`wait4` loop, so forked children are
//!   inside the window by construction. There are no per-process counters to
//!   misattribute, which is what turned a 242 ns crossing into a reported 378 us.
//! * **The crossing cost is a slope**, not a wall-clock ratio: two spawn phases that
//!   differ only in the number of guest syscalls each child issues, differenced.
//! * **The results directory must not exist.** A resumable ledger keyed on a reused path
//!   replays cached rows and prints a clean pass without measuring anything.
//! * **Arm order is balanced.** Each round rotates and alternates the arm order, because a
//!   fixed order puts a uniform +4% on whichever arm runs second on this box.
//! * **The engine's identity is the `.so`.** The C engine ships as a dlopened
//!   `libhl_native_engine.so`; the worker executables are identical by construction, so
//!   hashing them proves nothing.

use super::{definition::artifact_identity, evidence::Measurement};
use crate::suite::Error;
use clap::Args;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Arms are ordered base-first; the schedule rotates and reverses them per round.
const ARMS: [Arm; 3] = [Arm::Native, Arm::Engine, Arm::EngineNull];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Arm {
    /// The bare host kernel running the identical static guest image. The baseline.
    Native,
    /// The integrated C engine.
    Engine,
    /// The integrated C engine again, same binary and same options. Its ratio against
    /// `Engine` is the measured floor of what this harness can resolve: an effect
    /// smaller than the null arm's spread is not evidence.
    EngineNull,
}

impl Arm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Engine => "engine",
            Self::EngineNull => "engine-null",
        }
    }
}

#[derive(Args)]
pub(crate) struct Options {
    /// Result directory. It must not already exist: a reused path is how a run silently
    /// replays cached rows instead of measuring.
    #[arg(long)]
    results: PathBuf,
    /// Engine worker for the host ISA. Its sibling `libhl_native_engine.so` is the
    /// identity that gets hashed.
    #[arg(long)]
    engine: PathBuf,
    /// Static guest driver built from `tests/bench/floor/main.c`.
    #[arg(long)]
    guest: PathBuf,
    /// Directory used as the guest root. The driver is staged at `bin/floor` inside it.
    #[arg(long)]
    rootfs: PathBuf,
    /// Processes spawned per spawn phase.
    #[arg(long, default_value_t = 200)]
    execs: u64,
    /// Guest syscalls each child issues in the loaded spawn phase.
    #[arg(long, default_value_t = 100_000)]
    syscalls: u64,
    /// Guest arithmetic iterations in the control phase.
    #[arg(long, default_value_t = 20_000_000)]
    spin: u64,
    /// Balanced rounds. Must be even so every arm order is run in both directions.
    #[arg(long, default_value_t = 4, value_parser = parse_rounds)]
    rounds: u64,
    /// Consecutive quiet seconds required before requesting the box lock.
    #[arg(long, default_value_t = 60)]
    quiet_seconds: u64,
    /// Bounded wait for quiet and the lock. On expiry the run proceeds and records the load.
    #[arg(long, default_value_t = 240)]
    lock_timeout: u64,
    /// Maximum accepted one-minute host load while waiting.
    #[arg(long, default_value_t = 1.5)]
    max_load: f64,
}

fn parse_rounds(value: &str) -> Result<u64, String> {
    match value.parse::<u64>() {
        Ok(value) if value >= 2 && value.is_multiple_of(2) => Ok(value),
        _ => Err("rounds must be an even count of at least 2".into()),
    }
}

/// One measured cell: minimum microseconds observed for an arm and phase.
struct Samples(BTreeMap<(Arm, &'static str), u64>);

impl Samples {
    fn record(&mut self, arm: Arm, phase: &'static str, micros: u64) {
        self.0
            .entry((arm, phase))
            .and_modify(|best| *best = (*best).min(micros))
            .or_insert(micros);
    }

    fn get(&self, arm: Arm, phase: &'static str) -> Result<u64, Error> {
        self.0
            .get(&(arm, phase))
            .copied()
            .ok_or_else(|| format!("floor benchmark never measured {}/{phase}", arm.as_str()).into())
    }
}

pub(crate) fn run(options: Options) -> Result<(), Error> {
    if options.results.exists() {
        return Err(format!(
            "floor benchmark refuses to reuse {}: give every run a fresh results path",
            options.results.display()
        )
        .into());
    }
    fs::create_dir_all(&options.results)?;
    let library = engine_library(&options.engine)?;
    let staged = options.rootfs.join("bin/floor");
    fs::create_dir_all(options.rootfs.join("bin"))?;
    fs::copy(&options.guest, &staged)?;

    let identity = format!(
        "engine-library={}\nengine-worker={}\nguest={}\n",
        artifact_identity(&library)?,
        artifact_identity(&options.engine)?,
        artifact_identity(&staged)?
    );

    let held = Measurement::acquire(options.quiet_seconds, options.lock_timeout, options.max_load);
    let lock = match &held {
        Ok(_) => "box lock HELD exclusive (fd 8 announced, then fd 9)".to_owned(),
        Err(error) => format!("box lock NOT obtained ({error}); proceeding and recording the load"),
    };
    let load = load_average()?;

    let mut samples = Samples(BTreeMap::new());
    for round in 0..options.rounds {
        for arm in schedule(round) {
            for (phase, arguments) in phases(&options) {
                let micros = measure(arm, &options, &arguments)?;
                samples.record(arm, phase, micros);
            }
        }
    }

    let report = report(&options, &samples, &identity, &lock, &load)?;
    fs::write(options.results.join("report.txt"), &report)?;
    fs::write(options.results.join("identity.txt"), &identity)?;
    print!("{report}");
    drop(held);
    Ok(())
}

/// Rotate the arm order by the round, and reverse it on odd rounds. A fixed order
/// survives pinning, minima and per-arm verification, and still inflates whichever arm
/// runs last by about 4% on this box.
fn schedule(round: u64) -> Vec<Arm> {
    let count = ARMS.len() as u64;
    let mut order: Vec<Arm> = (0..count)
        .map(|index| ARMS[usize::try_from((index + round) % count).expect("arm index fits")])
        .collect();
    if !round.is_multiple_of(2) {
        order.reverse();
    }
    order
}

/// Every phase this harness knows how to run. All of them are reported, always.
fn phases(options: &Options) -> Vec<(&'static str, Vec<String>)> {
    vec![
        // Fixed per-process cost: fork + execve + wait of a static guest whose child
        // issues no syscalls at all beyond its own exit.
        ("proc", vec!["spawn".into(), options.execs.to_string(), "0".into()]),
        // The same loop with a known number of guest syscalls added per child. The
        // difference against `proc` is N*K crossings and nothing else.
        (
            "proc-loaded",
            vec!["spawn".into(), options.execs.to_string(), options.syscalls.to_string()],
        ),
        // Control: pure guest arithmetic, no fork, no execve, no syscall in the timed
        // region. Nothing in the host-side exec path can reach it.
        ("spin", vec!["spin".into(), options.spin.to_string()]),
    ]
}

fn measure(arm: Arm, options: &Options, arguments: &[String]) -> Result<u64, Error> {
    let mut command = match arm {
        Arm::Native => {
            let mut command = Command::new(options.rootfs.join("bin/floor"));
            command.args(arguments);
            command
        }
        Arm::Engine | Arm::EngineNull => {
            let mut command = Command::new(&options.engine);
            command
                .arg("--rootfs")
                .arg(&options.rootfs)
                .arg("bin/floor")
                .args(arguments);
            command
        }
    };
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "floor benchmark arm {} failed: status={} stderr={}",
            arm.as_str(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    phase_micros(&String::from_utf8_lossy(&output.stdout))
}

/// The guest emits exactly one `PHASE <name> us=<n> ok=<n>` line. `ok` is a work proof:
/// a zero or missing one means the phase did not run, which must not read as a fast arm.
fn phase_micros(stdout: &str) -> Result<u64, Error> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("PHASE "))
        .ok_or_else(|| Error::from(format!("floor guest emitted no PHASE line: {stdout:?}")))?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [_, _, micros, ok] = fields.as_slice() else {
        return Err(format!("floor guest PHASE line has an unexpected shape: {line:?}").into());
    };
    let micros: u64 = micros
        .strip_prefix("us=")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::from(format!("floor guest PHASE line has no us= field: {line:?}")))?;
    let ok: u64 = ok
        .strip_prefix("ok=")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::from(format!("floor guest PHASE line has no ok= field: {line:?}")))?;
    if ok == 0 {
        return Err(format!("floor guest reported no completed work: {line:?}").into());
    }
    Ok(micros)
}

/// The C engine is a dlopened shared object beside the worker. Comparing worker hashes is
/// meaningless because they are identical by construction.
fn engine_library(engine: &Path) -> Result<PathBuf, Error> {
    let directory = engine
        .parent()
        .ok_or_else(|| Error::from("engine worker path has no directory"))?;
    let library = directory.join("libhl_native_engine.so");
    if library.exists() {
        Ok(library)
    } else {
        Err(format!(
            "engine library {} is missing; the worker hash alone is not an engine identity",
            library.display()
        )
        .into())
    }
}

fn load_average() -> Result<String, Error> {
    Ok(fs::read_to_string("/proc/loadavg")?
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join("/"))
}

#[expect(clippy::cast_precision_loss, reason = "microsecond counts are far below 2^53")]
fn report(options: &Options, samples: &Samples, identity: &str, lock: &str, load: &str) -> Result<String, Error> {
    let execs = options.execs as f64;
    let crossings = execs * options.syscalls as f64;
    let mut text = String::new();
    text.push_str("FLOOR BENCHMARK\n");
    text.push_str(identity);
    text.push_str(&format!("{lock}\nload {load}\n"));
    text.push_str(&format!(
        "rounds={} execs={} syscalls-per-child={} spin={} (minimum across rounds, balanced order)\n\n",
        options.rounds, options.execs, options.syscalls, options.spin
    ));
    text.push_str("| phase | arm | total us | derived |\n|---|---|---|---|\n");
    for arm in ARMS {
        for (phase, _) in phases(options) {
            let micros = samples.get(arm, phase)?;
            let derived = match phase {
                "proc" => format!("{:.3} ms/exec", micros as f64 / execs / 1000.0),
                "proc-loaded" => format!(
                    "{:.1} ns/crossing",
                    (micros as f64 - samples.get(arm, "proc")? as f64) * 1000.0 / crossings
                ),
                _ => "control".into(),
            };
            text.push_str(&format!("| {phase} | {} | {micros} | {derived} |\n", arm.as_str()));
        }
    }
    text.push('\n');
    let ratio = |arm: Arm, phase: &'static str| -> Result<f64, Error> {
        Ok(samples.get(arm, phase)? as f64 / samples.get(Arm::Native, phase)? as f64)
    };
    text.push_str(&format!(
        "engine/native  proc {:.2}x  proc-loaded {:.2}x  spin(control) {:.3}x\n",
        ratio(Arm::Engine, "proc")?,
        ratio(Arm::Engine, "proc-loaded")?,
        ratio(Arm::Engine, "spin")?
    ));
    let null = |phase: &'static str| -> Result<f64, Error> {
        Ok(samples.get(Arm::EngineNull, phase)? as f64 / samples.get(Arm::Engine, phase)? as f64)
    };
    text.push_str(&format!(
        "null arm (engine vs engine, same .so)  proc {:.3}  proc-loaded {:.3}  spin {:.3}\n",
        null("proc")?,
        null("proc-loaded")?,
        null("spin")?
    ));
    text.push_str("The null arm is the resolution floor: an effect inside its spread is not evidence.\n");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::{Arm, phase_micros, schedule};

    #[test]
    fn a_reused_results_path_is_the_default_refusal() {
        // The refusal itself is exercised by `run`; this pins the parser's own bound.
        assert!(super::parse_rounds("3").is_err());
        assert!(super::parse_rounds("0").is_err());
        assert_eq!(super::parse_rounds("4"), Ok(4));
    }

    #[test]
    fn arm_order_is_balanced_across_rounds() {
        // Every arm must occupy every position across a full even cycle, or whichever arm
        // runs last collects a uniform inflation that no other precaution detects.
        let mut positions = std::collections::BTreeMap::new();
        for round in 0..6 {
            for (position, arm) in schedule(round).into_iter().enumerate() {
                positions.entry(arm).or_insert_with(Vec::new).push(position);
            }
        }
        for arm in [Arm::Native, Arm::Engine, Arm::EngineNull] {
            let seen = positions.remove(&arm).unwrap();
            assert_eq!(seen.len(), 6);
            for position in 0..3 {
                assert!(
                    seen.contains(&position),
                    "{} never ran at position {position}",
                    arm.as_str()
                );
            }
        }
        assert_ne!(schedule(0), schedule(1), "round order never reversed");
    }

    #[test]
    fn a_phase_that_did_no_work_is_not_a_fast_phase() {
        assert_eq!(phase_micros("PHASE spawn us=1234 ok=200\n").unwrap(), 1234);
        for invalid in [
            "PHASE spawn us=1234 ok=0\n",
            "PHASE spawn ok=200 us=1234\n",
            "PHASE spawn us=1234\n",
            "no phase line at all\n",
        ] {
            assert!(phase_micros(invalid).is_err(), "accepted {invalid:?}");
        }
    }
}
