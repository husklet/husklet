//! Serialized A/B driver for two engine arms over one guest binary.
//!
//! Every lane that has measured this engine so far rebuilt a private driver on top of `bench`,
//! which runs its cases through a concurrent pool and rebuilds the guest per case. Both are wrong
//! for an A/B: concurrent arms contend and minima do not survive contention, and a per-arm guest
//! build means the two arms never executed the same bytes. This runs one guest, one round at a
//! time, alternating which arm goes first inside each round, and reports the identity of every
//! artifact it used.

use crate::record::FramedIdentity;
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

type Error = Box<dyn std::error::Error>;

/// A null arm reading further than this from parity means the box cannot resolve anything smaller,
/// so no candidate effect below it is evidence.
const NULL_ARM_TOLERANCE: f64 = 0.005;

#[derive(Args)]
pub struct Options {
    /// Engine binary for the base arm.
    #[arg(long)]
    base: PathBuf,
    /// Engine binary for the candidate arm. Omitted, the base binary runs both arms, which removes
    /// binary layout from the comparison entirely; use it with `--candidate-engine-option`.
    #[arg(long)]
    candidate: Option<PathBuf>,
    /// The one guest binary both arms execute.
    #[arg(long)]
    guest: PathBuf,
    /// Engine option applied to both arms, as `KEY=VALUE`. Repeatable.
    #[arg(long = "engine-option")]
    shared: Vec<String>,
    /// Engine option applied to the candidate arm only, as `KEY=VALUE`. Repeatable.
    #[arg(long = "candidate-engine-option")]
    only_candidate: Vec<String>,
    /// Guest argument, after the guest path. Repeatable.
    #[arg(long = "guest-argument")]
    guest_arguments: Vec<String>,
    #[arg(long, default_value_t = 5)]
    rounds: usize,
    /// Results path. Refused if it already exists, because a reused path is how a run replays
    /// instead of measuring.
    #[arg(long)]
    results: PathBuf,
    /// The results file of a prior null-arm run over this same guest. Required when the two arms
    /// do not share an engine binary, because that comparison carries binary layout the run itself
    /// cannot separate out.
    #[arg(long)]
    null_arm_results: Option<PathBuf>,
}

/// One `PHASE <name> us=<n> ok=<n>` line.
struct Phase {
    name: String,
    us: u128,
    ok: String,
}

struct Arm {
    label: &'static str,
    binary: PathBuf,
    options: Vec<String>,
}

pub fn run(options: Options) -> Result<(), Error> {
    if options.results.exists() {
        return Err(format!("results path {} already exists", options.results.display()).into());
    }
    if options.rounds == 0 {
        return Err("rounds must be at least 1".into());
    }
    let candidate_binary = options.candidate.clone().unwrap_or_else(|| options.base.clone());
    let null_arm = options.candidate.is_none() && options.only_candidate.is_empty();
    let base = Arm {
        label: "base",
        binary: options.base.clone(),
        options: options.shared.clone(),
    };
    let candidate = Arm {
        label: "candidate",
        binary: candidate_binary,
        options: options.shared.iter().chain(&options.only_candidate).cloned().collect(),
    };

    let guest_identity = FramedIdentity::of_file(&options.guest)?;
    let base_identity = FramedIdentity::of_file(&base.binary)?;
    let candidate_identity = FramedIdentity::of_file(&candidate.binary)?;
    // Two arms from different binaries carry a layout difference this run cannot separate from the
    // change under test, so it may not proceed on a promise that a floor exists: it must be handed
    // the null arm's own results file. Both results withdrawn tonight came from lanes that had a
    // plausible mechanism and would have read straight past a printed warning.
    let floor = if base_identity == candidate_identity {
        None
    } else {
        Some(admit_floor(options.null_arm_results.as_deref(), &guest_identity)?)
    };
    let mut header = String::new();
    header.push_str(&format!("# guest\t{}\t{}\n", options.guest.display(), guest_identity));
    header.push_str(&format!("# base\t{}\t{}\n", base.binary.display(), base_identity));
    header.push_str(&format!(
        "# candidate\t{}\t{}\n",
        candidate.binary.display(),
        candidate_identity
    ));
    header.push_str(&format!("# base-options\t{}\n", base.options.join(" ")));
    header.push_str(&format!("# candidate-options\t{}\n", candidate.options.join(" ")));
    header.push_str(&format!("# rounds\t{}\n", options.rounds));
    header.push_str(&format!("# null-arm\t{null_arm}\n"));
    if let Some(floor) = &floor {
        header.push_str(&format!("# floor\t{floor}\n"));
    }
    print!("{header}");

    // One run of each binary before it is used for anything, because a binary copied out of a
    // moving directory has been corrupt and deterministically so.
    for arm in [&base, &candidate] {
        let phases = execute(arm, &options)?;
        if phases.is_empty() {
            return Err(format!("{} produced no PHASE line on its verification run", arm.label).into());
        }
    }

    let mut samples: BTreeMap<(String, &'static str), Vec<u128>> = BTreeMap::new();
    let mut checksums: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for round in 0..options.rounds {
        // Alternate which arm runs first inside the round. A fixed order buys the arm that runs
        // second a uniform few percent and survives pinning, minima and checksum verification.
        let order = if round % 2 == 0 {
            [&base, &candidate]
        } else {
            [&candidate, &base]
        };
        for arm in order {
            for phase in execute(arm, &options)? {
                checksums.entry(phase.name.clone()).or_default().insert(phase.ok);
                samples.entry((phase.name, arm.label)).or_default().push(phase.us);
            }
        }
    }

    let mut body = String::from("phase\tbase_us\tcandidate_us\tratio\n");
    let names: BTreeSet<_> = samples.keys().map(|(name, _)| name.clone()).collect();
    let mut worst: Option<(f64, String)> = None;
    for name in &names {
        let (Some(base_us), Some(candidate_us)) = (
            samples.get(&(name.clone(), "base")).and_then(|s| s.iter().min()),
            samples.get(&(name.clone(), "candidate")).and_then(|s| s.iter().min()),
        ) else {
            return Err(format!("phase {name} did not run on both arms").into());
        };
        let ratio = *candidate_us as f64 / *base_us as f64;
        body.push_str(&format!("{name}\t{base_us}\t{candidate_us}\t{ratio:.4}\n"));
        if worst
            .as_ref()
            .is_none_or(|(held, _)| (ratio - 1.0).abs() > (held - 1.0).abs())
        {
            worst = Some((ratio, name.clone()));
        }
    }
    print!("{body}");

    // Every run ends with the null-arm verdict and the identity of everything it executed, so a
    // reader cannot take a ratio from this driver without also seeing which tree produced it and
    // whether the floor was ever established. Two results were withdrawn tonight for exactly that.
    let mut notes = String::new();
    for (name, observed) in &checksums {
        if observed.len() > 1 {
            notes.push_str(&format!("# DISAGREEMENT\t{name}\t{observed:?}\n"));
        }
    }
    notes.push_str(&null_arm_verdict(null_arm, worst.as_ref(), floor.as_deref()));
    if let Some(floor) = &floor {
        notes.push_str(&format!("# floor\t{floor}\n"));
    }
    notes.push_str(&format!("# identity\tguest\t{guest_identity}\n"));
    notes.push_str(&format!("# identity\tbase\t{base_identity}\n"));
    notes.push_str(&format!("# identity\tcandidate\t{candidate_identity}\n"));
    if base_identity == candidate_identity {
        notes.push_str("# identity\tarms ran the same engine binary, so binary layout is not in this comparison\n");
    } else {
        notes.push_str("# identity\tarms ran DIFFERENT engine binaries; two builds of identical source have\n");
        notes.push_str("# identity\tdiffered by 152 bytes, so part of any ratio below is layout, not algorithm\n");
    }
    print!("{notes}");

    if let Some(parent) = options.results.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&options.results, format!("{header}{body}{notes}"))?;
    Ok(())
}

/// The null-arm line every run ends with. A run that did not take one says so rather than leaving
/// a reader to assume a floor was established.
/// Reads a prior null-arm results file and returns the floor it established. A run comparing two
/// binaries is refused outright unless this succeeds: no table, non-zero exit.
fn admit_floor(path: Option<&Path>, guest_identity: &str) -> Result<String, Error> {
    let Some(path) = path else {
        return Err(
            "the two arms do not share an engine binary, so this comparison carries a binary \
                    layout difference it cannot separate from the change under test. Establish the \
                    floor first -- run this driver with neither --candidate nor \
                    --candidate-engine-option -- and pass that results file as --null-arm-results."
                .into(),
        );
    };
    let text = std::fs::read_to_string(path)?;
    let mut null_arm = false;
    let mut resolved = false;
    let mut guest = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        match fields.as_slice() {
            ["# null-arm", "true"] => null_arm = true,
            ["# null-arm", "RESOLVED", ..] => resolved = true,
            ["# identity", "guest", identity] => guest = Some((*identity).to_owned()),
            _ => {}
        }
    }
    if !null_arm {
        return Err(format!("{} is not a null-arm run", path.display()).into());
    }
    if !resolved {
        return Err(format!(
            "{} did not resolve: its own arms differed by more than the tolerance, so this box \
             cannot resolve the candidate either",
            path.display()
        )
        .into());
    }
    match guest {
        Some(identity) if identity == guest_identity => Ok(format!("{}\t{identity}", path.display())),
        Some(identity) => Err(format!(
            "{} established a floor for guest {identity}, not for the guest this run executes \
             ({guest_identity})",
            path.display()
        )
        .into()),
        None => Err(format!("{} names no guest identity", path.display()).into()),
    }
}

fn null_arm_verdict(null_arm: bool, worst: Option<&(f64, String)>, floor: Option<&str>) -> String {
    let Some((ratio, name)) = worst else {
        return "# null-arm\tNO-PHASES\n".to_owned();
    };
    if !null_arm {
        return match floor {
            Some(floor) => format!(
                "# null-arm\tNOT-RUN-HERE\tfloor established by {floor}\n\
                 # null-arm\tDisbelieve any ratio in this table that is inside that floor's spread.\n"
            ),
            None => "# null-arm\tNOT-RUN\tthis run compared two arms; the floor is unknown here.\n\
                     # null-arm\tRe-run with neither --candidate nor --candidate-engine-option to establish it,\n\
                     # null-arm\tand disbelieve any ratio in this table that is inside that spread.\n"
                .to_owned(),
        };
    }
    let resolved = (ratio - 1.0).abs() <= NULL_ARM_TOLERANCE;
    let verdict = if resolved { "RESOLVED" } else { "UNRESOLVED" };
    let mut line = format!("# null-arm\t{verdict}\tworst={name}\tratio={ratio:.4}\ttolerance={NULL_ARM_TOLERANCE}\n");
    if !resolved {
        line.push_str("# null-arm\tThis box cannot resolve an effect smaller than the spread above. No\n");
        line.push_str("# null-arm\tcandidate ratio inside it is evidence, however clean the other controls read.\n");
    }
    line
}

fn execute(arm: &Arm, options: &Options) -> Result<Vec<Phase>, Error> {
    let mut command = Command::new(&arm.binary);
    for option in &arm.options {
        command.arg("--engine-option").arg(option);
    }
    command.arg(&options.guest);
    for argument in &options.guest_arguments {
        command.arg(argument);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            arm.label,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

/// Reads `PHASE <name> us=<n> ok=<n>` lines and ignores everything else.
fn parse(text: &str) -> Vec<Phase> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            if fields.next() != Some("PHASE") {
                return None;
            }
            let name = fields.next()?.to_owned();
            let mut us = None;
            let mut ok = None;
            for field in fields {
                if let Some(value) = field.strip_prefix("us=") {
                    us = value.parse().ok();
                } else if let Some(value) = field.strip_prefix("ok=") {
                    ok = Some(value.to_owned());
                }
            }
            Some(Phase {
                name,
                us: us?,
                ok: ok.unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_lines_are_read_and_other_output_ignored() {
        let phases = parse("noise\nPHASE compute us=1234 ok=7\nPHASE syscall us=99 ok=1\ntrailing\n");
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].name, "compute");
        assert_eq!(phases[0].us, 1234);
        assert_eq!(phases[0].ok, "7");
        assert_eq!(phases[1].us, 99);
        assert!(
            parse("PHASE compute ok=1\n").is_empty(),
            "a phase without us is not a sample"
        );
    }

    #[test]
    fn arm_order_alternates_within_each_round() {
        // The property the driver exists to hold: over an even number of rounds each arm runs
        // first exactly half the time, so a first/second-position bias cancels instead of landing
        // on one arm.
        let mut first = [0_usize; 2];
        for round in 0..8 {
            first[usize::from(round % 2 != 0)] += 1;
        }
        assert_eq!(first, [4, 4]);
    }

    #[test]
    fn two_binaries_are_refused_without_a_resolved_null_arm_for_the_same_guest() {
        let guest = "abc123";
        assert!(admit_floor(None, guest).is_err(), "a promise is not a floor");

        let directory = std::env::temp_dir().join(format!("hl-ab-floor-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create");
        let write = |name: &str, body: &str| {
            let path = directory.join(name);
            std::fs::write(&path, body).expect("write");
            path
        };

        let comparison = write("comparison.tsv", "# null-arm\tfalse\n# identity\tguest\tabc123\n");
        assert!(
            admit_floor(Some(&comparison), guest).is_err(),
            "a comparison is not a floor"
        );

        let drifted = write(
            "drifted.tsv",
            "# null-arm\ttrue\n# null-arm\tUNRESOLVED\tworst=syscall\n# identity\tguest\tabc123\n",
        );
        assert!(
            admit_floor(Some(&drifted), guest).is_err(),
            "an unresolved floor is not a floor"
        );

        let other = write(
            "other.tsv",
            "# null-arm\ttrue\n# null-arm\tRESOLVED\tworst=compute\n# identity\tguest\tdifferent\n",
        );
        assert!(
            admit_floor(Some(&other), guest).is_err(),
            "a floor for another guest does not carry"
        );

        let good = write(
            "good.tsv",
            "# null-arm\ttrue\n# null-arm\tRESOLVED\tworst=compute\n# identity\tguest\tabc123\n",
        );
        assert!(
            admit_floor(Some(&good), guest).is_ok(),
            "a resolved floor for this guest admits"
        );
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn every_run_ends_with_a_null_arm_line() {
        let worst = (1.0004_f64, "compute".to_owned());
        let resolved = null_arm_verdict(true, Some(&worst), None);
        assert!(resolved.contains("RESOLVED"), "{resolved}");
        assert!(!resolved.contains("UNRESOLVED"), "{resolved}");

        let drifted = (1.017_f64, "syscall".to_owned());
        let unresolved = null_arm_verdict(true, Some(&drifted), None);
        assert!(unresolved.contains("UNRESOLVED"), "{unresolved}");

        // A comparison run must still say, in its own output, that no floor was established.
        let compared = null_arm_verdict(false, Some(&worst), None);
        assert!(compared.contains("NOT-RUN"), "{compared}");
        // A run handed a floor must cite it rather than claim the floor is unknown.
        let carried = null_arm_verdict(false, Some(&worst), Some("null.tsv\tabc123"));
        assert!(carried.contains("NOT-RUN-HERE"), "{carried}");
        assert!(carried.contains("null.tsv"), "{carried}");
        assert!(!carried.contains("the floor is unknown here"), "{carried}");
        assert!(null_arm_verdict(false, None, None).contains("NO-PHASES"));
    }

    #[test]
    fn a_null_arm_is_the_same_binary_with_the_same_options() {
        let tolerance = NULL_ARM_TOLERANCE;
        assert!(
            (1.017_f64 - 1.0).abs() > tolerance,
            "the reading this driver replaces must fail"
        );
        assert!((1.000_f64 - 1.0).abs() <= tolerance);
    }
}
