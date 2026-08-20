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

use super::{evidence::Measurement, identity::artifact_identity};
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

/// A candidate run answers a different question -- one engine build against another -- so the
/// bare-host baseline is not one of its arms. Both arms are engines, and the null arm's job of
/// bounding the resolution is done instead by the control phase and by the two arms differing
/// only in the `.so` beside an identical worker.
const CANDIDATE_ARMS: [Arm; 2] = [Arm::Engine, Arm::EngineCandidate];

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
    /// A second engine build, given as `--engine-candidate`. Present only in a candidate run,
    /// where it replaces both `Native` and `EngineNull`.
    EngineCandidate,
}

impl Arm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Engine => "engine",
            Self::EngineNull => "engine-null",
            Self::EngineCandidate => "engine-candidate",
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
    /// A second engine worker to measure `--engine` against. Given it, the run compares the two
    /// engine builds directly and drops the native and null arms; its sibling
    /// `libhl_native_engine.so` is hashed beside the first, because the workers are identical.
    #[arg(long)]
    engine_candidate: Option<PathBuf>,
    /// A dynamically linked victim, already inside the rootfs, named by its guest-visible path.
    /// It adds an `image` phase: a dynamic guest carries a program interpreter as a second exec
    /// image, which the static driver does not, so the exec path does strictly more work for it.
    #[arg(long)]
    dynamic_victim: Option<String>,
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

    /// Measures every phase once for one arm, keeping the minimum per phase.
    fn measure_phases(&mut self, arm: Arm, options: &Options) -> Result<(), Error> {
        for (phase, arguments) in phases(options) {
            let micros = measure(arm, options, &arguments)?;
            self.record(arm, phase, micros);
        }
        Ok(())
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

    let mut identity = format!(
        "engine-library={}\nengine-worker={}\nguest={}\n",
        artifact_identity(&library)?,
        artifact_identity(&options.engine)?,
        artifact_identity(&staged)?
    );
    if let Some(candidate) = &options.engine_candidate {
        let candidate_library = engine_library(candidate)?;
        identity.push_str(&format!(
            "candidate-library={}\ncandidate-worker={}\n",
            artifact_identity(&candidate_library)?,
            artifact_identity(candidate)?
        ));
        if artifact_identity(&candidate_library)? == artifact_identity(&library)? {
            return Err(
                "the two arms carry the same libhl_native_engine.so: there is nothing to compare"
                    .to_owned()
                    .into(),
            );
        }
    }

    let held = Measurement::acquire(options.quiet_seconds, options.lock_timeout, options.max_load);
    let lock = match &held {
        Ok(_) => "box lock HELD exclusive (fd 8 announced, then fd 9)".to_owned(),
        Err(error) => format!("box lock NOT obtained ({error}); proceeding and recording the load"),
    };
    let load = load_average()?;

    let mut samples = Samples(BTreeMap::new());
    for round in 0..options.rounds {
        for arm in schedule(round, &arms(&options)) {
            samples.measure_phases(arm, &options)?;
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
fn arms(options: &Options) -> Vec<Arm> {
    if options.engine_candidate.is_some() {
        CANDIDATE_ARMS.to_vec()
    } else {
        ARMS.to_vec()
    }
}

fn schedule(round: u64, arms: &[Arm]) -> Vec<Arm> {
    let count = arms.len() as u64;
    // Rotate on every second round, reverse on the odd ones. Rotating every round instead
    // cancels against the reversal at two arms -- `[base, cand]` rotated once and reversed is
    // `[base, cand]` again -- so a two-arm candidate run would have held a fixed order while
    // looking balanced, which is exactly the failure this schedule exists to prevent.
    let mut order: Vec<Arm> = (0..count)
        .map(|index| arms[usize::try_from((index + round / 2) % count).expect("arm index fits")])
        .collect();
    if !round.is_multiple_of(2) {
        order.reverse();
    }
    order
}

/// Every phase this harness knows how to run. All of them are reported, always.
fn phases(options: &Options) -> Vec<(&'static str, Vec<String>)> {
    let mut rows = vec![
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
    ];
    if let Some(victim) = &options.dynamic_victim {
        // Kept last so the static phases stay comparable with a run that did not ask for it.
        rows.push(("image", vec!["image".into(), options.execs.to_string(), victim.clone()]));
    }
    rows
}

fn measure(arm: Arm, options: &Options, arguments: &[String]) -> Result<u64, Error> {
    let mut command = match arm {
        Arm::Native => {
            let mut command = Command::new(options.rootfs.join("bin/floor"));
            command.args(arguments);
            command
        }
        Arm::Engine | Arm::EngineNull | Arm::EngineCandidate => {
            let worker = if arm == Arm::EngineCandidate {
                options
                    .engine_candidate
                    .as_ref()
                    .ok_or_else(|| Error::from("the candidate arm was scheduled without --engine-candidate"))?
            } else {
                &options.engine
            };
            let mut command = Command::new(worker);
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
    for arm in arms(options) {
        for (phase, _) in phases(options) {
            let micros = samples.get(arm, phase)?;
            let derived = match phase {
                "proc" | "image" => format!("{:.3} ms/exec", micros as f64 / execs / 1000.0),
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
    // Every phase is reported against the run's own reference arm, never a subset: a table that
    // lists the phases that moved and omits the ones that did not is not evidence.
    let against = |arm: Arm, reference: Arm| -> Result<String, Error> {
        let mut row = String::new();
        for (phase, _) in phases(options) {
            let ratio = samples.get(arm, phase)? as f64 / samples.get(reference, phase)? as f64;
            row.push_str(&format!("  {phase} {ratio:.3}"));
        }
        Ok(row)
    };
    if options.engine_candidate.is_some() {
        text.push_str(&format!(
            "candidate/base (two engine builds, identical worker){}\n",
            against(Arm::EngineCandidate, Arm::Engine)?
        ));
        text.push_str(
            "`spin` is the control: the exec path cannot reach it, so a candidate effect it also \
             shows is the harness, not the change.\n",
        );
    } else {
        text.push_str(&format!("engine/native{}\n", against(Arm::Engine, Arm::Native)?));
        text.push_str(&format!(
            "null arm (engine vs engine, same .so){}\n",
            against(Arm::EngineNull, Arm::Engine)?
        ));
        text.push_str("The null arm is the resolution floor: an effect inside its spread is not evidence.\n");
    }
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
            for (position, arm) in schedule(round, &super::ARMS).into_iter().enumerate() {
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
        assert_ne!(
            schedule(0, &super::ARMS),
            schedule(1, &super::ARMS),
            "round order never reversed"
        );
    }

    #[test]
    fn a_candidate_run_still_balances_its_two_engine_arms() {
        for round in 0..4 {
            assert_eq!(schedule(round, &super::CANDIDATE_ARMS).len(), 2);
        }
        assert_ne!(
            schedule(0, &super::CANDIDATE_ARMS),
            schedule(1, &super::CANDIDATE_ARMS),
            "candidate arm order never reversed"
        );
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
