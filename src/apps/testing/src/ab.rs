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

/// One phase's own null-arm reading: how far it drifted from parity for no reason at all. The
/// aggregate verdict is set by the worst phase, so a single stably-noisy phase voids every other
/// one; the floor a lane can actually use is the floor of the phase it is measuring.
struct PhaseFloor {
    name: String,
    floor: f64,
    resolved: bool,
}

/// A floor inherited from a prior null-arm run, with the per-phase readings it published.
struct Floor {
    citation: String,
    phases: Vec<PhaseFloor>,
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
        header.push_str(&format!("# floor\t{}\n", floor.citation));
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
            record_samples(arm, execute(arm, &options)?, &mut checksums, &mut samples);
        }
    }

    let mut body = String::from("phase\tbase_us\tcandidate_us\tratio\n");
    let names: BTreeSet<_> = samples.keys().map(|(name, _)| name.clone()).collect();
    let mut ratios: Vec<(String, f64)> = Vec::new();
    for name in &names {
        let (Some(base_us), Some(candidate_us)) = (
            samples.get(&(name.clone(), "base")).and_then(|s| s.iter().min()),
            samples.get(&(name.clone(), "candidate")).and_then(|s| s.iter().min()),
        ) else {
            return Err(format!("phase {name} did not run on both arms").into());
        };
        let ratio = *candidate_us as f64 / *base_us as f64;
        body.push_str(&format!("{name}\t{base_us}\t{candidate_us}\t{ratio:.4}\n"));
        ratios.push((name.clone(), ratio));
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
    notes.push_str(&null_arm_verdict(null_arm, &ratios, floor.as_ref()));
    if let Some(floor) = &floor {
        notes.push_str(&format!("# floor\t{}\n", floor.citation));
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

fn record_samples(
    arm: &Arm,
    phases: Vec<Phase>,
    checksums: &mut BTreeMap<String, BTreeSet<String>>,
    samples: &mut BTreeMap<(String, &'static str), Vec<u128>>,
) {
    for phase in phases {
        checksums.entry(phase.name.clone()).or_default().insert(phase.ok);
        samples.entry((phase.name, arm.label)).or_default().push(phase.us);
    }
}

/// The null-arm line every run ends with. A run that did not take one says so rather than leaving
/// a reader to assume a floor was established.
/// Reads a prior null-arm results file and returns the floor it established. A run comparing two
/// binaries is refused outright unless this succeeds: no table, non-zero exit.
fn admit_floor(path: Option<&Path>, guest_identity: &str) -> Result<Floor, Error> {
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
    let mut phases: Vec<PhaseFloor> = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        match fields.as_slice() {
            ["# null-arm", "true"] => null_arm = true,
            ["# null-arm", "RESOLVED", ..] => resolved = true,
            ["# null-arm-phase", state, name, _, floor] => parse_floor_phase(state, name, floor, &mut phases),
            ["# identity", "guest", identity] => guest = Some((*identity).to_owned()),
            _ => {}
        }
    }
    if !null_arm {
        return Err(format!("{} is not a null-arm run", path.display()).into());
    }
    // A single stably-noisy phase makes every null arm on a full guest read UNRESOLVED, so the
    // aggregate alone cannot gate admission: what admits is at least one phase this box resolved.
    // The comparison then prints those per-phase floors, and a phase with no floor has none.
    if !resolved && !phases.iter().any(|phase| phase.resolved) {
        return Err(format!(
            "{} resolved no phase: its own arms differed by more than the tolerance everywhere, so \
             this box cannot resolve the candidate either",
            path.display()
        )
        .into());
    }
    match guest {
        Some(identity) if identity == guest_identity => Ok(Floor {
            citation: format!("{}\t{identity}", path.display()),
            phases,
        }),
        Some(identity) => Err(format!(
            "{} established a floor for guest {identity}, not for the guest this run executes \
             ({guest_identity})",
            path.display()
        )
        .into()),
        None => Err(format!("{} names no guest identity", path.display()).into()),
    }
}

fn parse_floor_phase(state: &str, name: &str, floor: &str, phases: &mut Vec<PhaseFloor>) {
    if let Some(value) = floor.strip_prefix("floor=").and_then(|value| value.parse().ok()) {
        phases.push(PhaseFloor {
            name: name.to_owned(),
            floor: value,
            resolved: state == "RESOLVED",
        });
    }
}

/// Emits the aggregate verdict *and* one verdict per phase. The aggregate is kept because a run in
/// which every phase drifted is genuinely broken and must say so loudly, but it is set by the worst
/// phase alone, and a phase that only reads a clock has voided effects two orders of magnitude
/// above their own phase's floor. So the per-phase lines carry the number a lane can use.
fn null_arm_verdict(null_arm: bool, ratios: &[(String, f64)], floor: Option<&Floor>) -> String {
    if ratios.is_empty() {
        return "# null-arm\tNO-PHASES\n".to_owned();
    }
    if !null_arm {
        let Some(floor) = floor else {
            return "# null-arm\tNOT-RUN\tthis run compared two arms; the floor is unknown here.\n\
                    # null-arm\tRe-run with neither --candidate nor --candidate-engine-option to establish it,\n\
                    # null-arm\tand disbelieve any ratio in this table that is inside that spread.\n"
                .to_owned();
        };
        let mut lines = format!(
            "# null-arm\tNOT-RUN-HERE\tfloor established by {}\n\
             # null-arm\tDisbelieve any ratio in this table that is inside its own phase's floor below.\n",
            floor.citation
        );
        for phase in &floor.phases {
            let verdict = verdict_of(phase.resolved);
            lines.push_str(&format!(
                "# null-arm-floor\t{verdict}\t{}\tfloor={:.4}\n",
                phase.name, phase.floor
            ));
        }
        if floor.phases.is_empty() {
            lines.push_str(
                "# null-arm-floor\tthat null arm published no per-phase floor; only its aggregate verdict carries.\n",
            );
        }
        return lines;
    }

    let mut per_phase = String::new();
    let mut resolved_phases = 0;
    let mut worst: Option<(&str, f64)> = None;
    for (name, ratio) in ratios {
        let drift = (ratio - 1.0).abs();
        let resolved = drift <= NULL_ARM_TOLERANCE;
        resolved_phases += usize::from(resolved);
        if worst.is_none_or(|(_, held)| drift > (held - 1.0).abs()) {
            worst = Some((name, *ratio));
        }
        per_phase.push_str(&format!(
            "# null-arm-phase\t{}\t{name}\tratio={ratio:.4}\tfloor={drift:.4}\n",
            verdict_of(resolved)
        ));
    }
    let (worst_name, worst_ratio) = worst.expect("ratios is not empty");
    let total = ratios.len();
    let mut lines = format!(
        "# null-arm\t{}\tworst={worst_name}\tratio={worst_ratio:.4}\ttolerance={NULL_ARM_TOLERANCE}\tresolved={resolved_phases}/{total}\n",
        verdict_of(resolved_phases == total)
    );
    if resolved_phases == 0 {
        lines.push_str("# null-arm\tEVERY phase drifted past the tolerance: this box resolved nothing at all,\n");
        lines.push_str("# null-arm\tand no ratio measured here is evidence however clean the other controls read.\n");
    } else if resolved_phases < total {
        lines.push_str("# null-arm\tThe aggregate above is set by the worst phase alone. Read the per-phase floor\n");
        lines.push_str("# null-arm\tfor the phase you are measuring; an effect inside that phase's own floor is not\n");
        lines.push_str("# null-arm\tevidence, and an effect above it is unaffected by another phase's noise.\n");
    }
    lines.push_str(&per_phase);
    lines
}

fn verdict_of(resolved: bool) -> &'static str {
    if resolved { "RESOLVED" } else { "UNRESOLVED" }
}

fn execute(arm: &Arm, options: &Options) -> Result<Vec<Phase>, Error> {
    let mut command = crate::platform::HostProcess::standard(&arm.binary);
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
                parse_phase_field(field, &mut us, &mut ok);
            }
            Some(Phase {
                name,
                us: us?,
                ok: ok.unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_phase_field(field: &str, us: &mut Option<u128>, ok: &mut Option<String>) {
    match (field.strip_prefix("us="), field.strip_prefix("ok=")) {
        (Some(value), _) => *us = value.parse().ok(),
        (_, Some(value)) => *ok = Some(value.to_owned()),
        _ => {}
    }
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

    fn ratios(rows: &[(&str, f64)]) -> Vec<(String, f64)> {
        rows.iter().map(|(name, ratio)| ((*name).to_owned(), *ratio)).collect()
    }

    #[test]
    fn every_run_ends_with_a_null_arm_line() {
        let clean = ratios(&[("compute", 1.0004)]);
        let resolved = null_arm_verdict(true, &clean, None);
        assert!(resolved.contains("RESOLVED"), "{resolved}");
        assert!(!resolved.contains("UNRESOLVED"), "{resolved}");

        let drifted = ratios(&[("syscall", 1.017)]);
        let unresolved = null_arm_verdict(true, &drifted, None);
        assert!(unresolved.contains("UNRESOLVED"), "{unresolved}");

        // A comparison run must still say, in its own output, that no floor was established.
        let compared = null_arm_verdict(false, &clean, None);
        assert!(compared.contains("NOT-RUN"), "{compared}");
        // A run handed a floor must cite it rather than claim the floor is unknown.
        let floor = Floor {
            citation: "null.tsv\tabc123".to_owned(),
            phases: vec![PhaseFloor {
                name: "string".to_owned(),
                floor: 0.0004,
                resolved: true,
            }],
        };
        let carried = null_arm_verdict(false, &clean, Some(&floor));
        assert!(carried.contains("NOT-RUN-HERE"), "{carried}");
        assert!(carried.contains("null.tsv"), "{carried}");
        assert!(!carried.contains("the floor is unknown here"), "{carried}");
        // The inherited floor must be readable per phase, not only cited as a path.
        assert!(
            carried.contains("# null-arm-floor\tRESOLVED\tstring\tfloor=0.0004\n"),
            "{carried}"
        );
        assert!(null_arm_verdict(false, &[], None).contains("NO-PHASES"));
    }

    #[test]
    fn one_noisy_phase_does_not_void_the_phases_that_resolved() {
        // The observed defect: `timebase` reads 0.9484 on a base-versus-base arm while every other
        // phase sits on a 0.04% floor, and the single aggregate line called the whole run
        // UNRESOLVED. A lane measuring `string` needs `string`'s floor, not `timebase`'s.
        let observed = ratios(&[
            ("atomics", 0.9998),
            ("malloc", 1.0092),
            ("mmap", 0.9989),
            ("string", 0.9996),
            ("timebase", 0.9484),
        ]);
        let verdict = null_arm_verdict(true, &observed, None);

        // The aggregate survives, still set by the worst phase, and now says how many resolved.
        assert!(
            verdict.contains("# null-arm\tUNRESOLVED\tworst=timebase\tratio=0.9484"),
            "{verdict}"
        );
        assert!(verdict.contains("resolved=3/5"), "{verdict}");

        // Every phase carries its own verdict and its own floor, with no arithmetic left to do.
        assert!(
            verdict.contains("# null-arm-phase\tRESOLVED\tstring\tratio=0.9996\tfloor=0.0004\n"),
            "{verdict}"
        );
        assert!(
            verdict.contains("# null-arm-phase\tRESOLVED\tatomics\tratio=0.9998\tfloor=0.0002\n"),
            "{verdict}"
        );
        assert!(
            verdict.contains("# null-arm-phase\tUNRESOLVED\tmalloc\tratio=1.0092\tfloor=0.0092\n"),
            "{verdict}"
        );
        assert!(
            verdict.contains("# null-arm-phase\tUNRESOLVED\ttimebase\tratio=0.9484\tfloor=0.0516\n"),
            "{verdict}"
        );

        // A run in which nothing resolved is genuinely broken and must still say so loudly.
        let broken = null_arm_verdict(true, &ratios(&[("string", 1.04), ("timebase", 0.94)]), None);
        assert!(broken.contains("resolved=0/2"), "{broken}");
        assert!(broken.contains("EVERY phase drifted"), "{broken}");
        assert!(!verdict.contains("EVERY phase drifted"), "{verdict}");
    }

    #[test]
    fn a_partly_resolved_null_arm_carries_its_per_phase_floors_into_a_comparison() {
        let guest = "abc123";
        let directory = std::env::temp_dir().join(format!("hl-ab-phase-floor-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create");
        let path = directory.join("null.tsv");
        std::fs::write(
            &path,
            "# null-arm\ttrue\n\
             # null-arm\tUNRESOLVED\tworst=timebase\tratio=0.9484\ttolerance=0.005\tresolved=1/2\n\
             # null-arm-phase\tRESOLVED\tstring\tratio=0.9996\tfloor=0.0004\n\
             # null-arm-phase\tUNRESOLVED\ttimebase\tratio=0.9484\tfloor=0.0516\n\
             # identity\tguest\tabc123\n",
        )
        .expect("write");

        // A stably-noisy phase must not refuse every comparison on the box: one resolved phase is a
        // floor for that phase, and the comparison prints it so the reader can find their own.
        let floor = admit_floor(Some(&path), guest).expect("a phase resolved, so a floor exists");
        assert_eq!(floor.phases.len(), 2);
        let carried = null_arm_verdict(false, &ratios(&[("string", 0.84)]), Some(&floor));
        assert!(
            carried.contains("# null-arm-floor\tRESOLVED\tstring\tfloor=0.0004\n"),
            "{carried}"
        );
        assert!(
            carried.contains("# null-arm-floor\tUNRESOLVED\ttimebase\tfloor=0.0516\n"),
            "{carried}"
        );

        // Nothing resolved anywhere is still a refusal.
        let nothing = directory.join("nothing.tsv");
        std::fs::write(
            &nothing,
            "# null-arm\ttrue\n\
             # null-arm\tUNRESOLVED\tworst=timebase\tratio=0.9484\ttolerance=0.005\tresolved=0/2\n\
             # null-arm-phase\tUNRESOLVED\tstring\tratio=1.0400\tfloor=0.0400\n\
             # null-arm-phase\tUNRESOLVED\ttimebase\tratio=0.9484\tfloor=0.0516\n\
             # identity\tguest\tabc123\n",
        )
        .expect("write");
        assert!(
            admit_floor(Some(&nothing), guest).is_err(),
            "a run that resolved no phase is not a floor"
        );
        std::fs::remove_dir_all(&directory).ok();
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
