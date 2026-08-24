use clap::Args;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, process::Command};

#[derive(Args)]
pub(crate) struct Options {
    /// Exact checkpoint_linux integration-test binary built from this repository tip.
    #[arg(long)]
    probe: PathBuf,
    /// Independent real checkpoint/restore rounds.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..=20))]
    samples: u32,
    /// Extra file mappings and descriptors held by the daily-development fixture.
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=512))]
    scale: u32,
    /// New directory that receives immutable per-sample ledgers.
    #[arg(long)]
    results: PathBuf,
}

#[derive(Clone)]
struct Row {
    component: String,
    isa: String,
    phase: String,
    duration_us: u64,
}

const NATIVE: [&str; 20] = [
    "peer_quiescence",
    "serialization",
    "settlement",
    "manifest_publication",
    "native_reap",
    "terminal",
    "restore_validation",
    "restore_resources_memory",
    "restore_process_commit",
    "terminal",
    "peer_quiescence",
    "serialization",
    "settlement",
    "manifest_publication",
    "native_reap",
    "terminal",
    "restore_validation",
    "restore_resources_memory",
    "restore_process_commit",
    "terminal",
];
const CONTROL: [&str; 14] = [
    "capture_ready_wait",
    "capture_admission",
    "request_dispatch",
    "completion_wait",
    "terminal",
    "recovery_wait",
    "terminal",
    "capture_ready_wait",
    "capture_admission",
    "request_dispatch",
    "completion_wait",
    "terminal",
    "recovery_wait",
    "terminal",
];

fn rows(ledger: &str) -> Result<Vec<Row>, String> {
    ledger
        .lines()
        .filter(|line| line.starts_with("checkpoint_phase_ledger\t"))
        .map(|line| {
            let fields = line
                .split('\t')
                .skip(1)
                .filter_map(|field| field.split_once('='))
                .collect::<BTreeMap<_, _>>();
            let keys = fields.keys().copied().collect::<Vec<_>>();
            let expected = [
                "attempt",
                "budget_us",
                "clock",
                "component",
                "duration_us",
                "generation",
                "isa",
                "outcome",
                "phase",
                "session",
                "status",
            ];
            if keys != expected {
                return Err(format!("phase row schema mismatch: {line}"));
            }
            let outcome_is_valid = if fields["phase"] == "terminal" {
                fields["outcome"] == "success"
            } else {
                fields["outcome"] == "progress"
            };
            if fields["clock"] != "ok" || !outcome_is_valid || fields["status"] != "0" {
                return Err(format!("checkpoint phase failed: {line}"));
            }
            Ok(Row {
                component: fields["component"].to_owned(),
                isa: fields["isa"].to_owned(),
                phase: fields["phase"].to_owned(),
                duration_us: fields["duration_us"]
                    .parse()
                    .map_err(|_| format!("invalid duration: {line}"))?,
            })
        })
        .collect()
}

fn validate(rows: &[Row]) -> Result<(), String> {
    for isa in ["aarch64", "x86_64"] {
        let native = rows
            .iter()
            .filter(|row| row.component == "native" && row.isa == isa)
            .map(|row| row.phase.as_str())
            .collect::<Vec<_>>();
        if native != NATIVE {
            return Err(format!("{isa} native phase order/count mismatch: {native:?}"));
        }
        let control = rows
            .iter()
            .filter(|row| row.component == "control" && row.isa == isa)
            .map(|row| row.phase.as_str())
            .collect::<Vec<_>>();
        if control != CONTROL {
            return Err(format!("{isa} control phase order/count mismatch: {control:?}"));
        }
    }
    if rows.iter().any(|row| {
        !matches!(row.component.as_str(), "native" | "control") || !matches!(row.isa.as_str(), "aarch64" | "x86_64")
    }) {
        return Err("unknown checkpoint component or ISA".to_owned());
    }
    Ok(())
}

pub(crate) fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    if options.results.exists() {
        return Err(format!("results path already exists: {}", options.results.display()).into());
    }
    if !options.probe.is_file() {
        return Err(format!("checkpoint probe is not a file: {}", options.probe.display()).into());
    }
    let temporary = options.results.with_extension(format!("tmp.{}", std::process::id()));
    if temporary.exists() {
        return Err(format!("temporary results path already exists: {}", temporary.display()).into());
    }
    std::fs::create_dir(&temporary)?;
    let probe_hash = Sha256::digest(std::fs::read(&options.probe)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    std::fs::write(temporary.join("probe.sha256"), format!("{probe_hash}\n"))?;
    let mut totals = BTreeMap::<(String, String, String), (u64, u64)>::new();
    for sample in 1..=options.samples {
        let ledger = temporary.join(format!("sample-{sample}.ledger"));
        let output = Command::new(&options.probe)
            .args([
                "--exact",
                "checkpoint_phase_ledger_probe_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("HL_CHECKPOINT_PHASE_LEDGER_PATH", &ledger)
            .env("HL_CHECKPOINT_PROFILE_SCALE", options.scale.to_string())
            .output()?;
        std::fs::write(temporary.join(format!("sample-{sample}.stdout")), &output.stdout)?;
        std::fs::write(temporary.join(format!("sample-{sample}.stderr")), &output.stderr)?;
        if !output.status.success() {
            return Err(format!("checkpoint sample {sample} failed with {}", output.status).into());
        }
        let content = std::fs::read_to_string(&ledger)?;
        let parsed = rows(&content).map_err(|error| format!("sample {sample}: {error}"))?;
        if parsed.is_empty() {
            return Err(format!("checkpoint sample {sample} published no phase rows").into());
        }
        validate(&parsed).map_err(|error| format!("sample {sample}: {error}"))?;
        for row in parsed {
            let total = totals.entry((row.component, row.isa, row.phase)).or_default();
            total.0 += row.duration_us;
            total.1 += 1;
        }
    }
    std::fs::write(
        temporary.join("receipt"),
        format!(
            "checkpoint_profile_v1\nsamples={}\nscale={}\nprobe_sha256={probe_hash}\n",
            options.samples, options.scale
        ),
    )?;
    std::fs::rename(&temporary, &options.results)?;
    println!(
        "checkpoint_profile_v1\tsamples={}\tscale={}",
        options.samples, options.scale
    );
    for ((component, isa, phase), (duration, count)) in totals {
        println!(
            "checkpoint_profile_phase\tcomponent={component}\tisa={isa}\tphase={phase}\tcount={count}\tmean_us={}",
            duration / count
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CONTROL, NATIVE, rows, validate};

    #[test]
    fn phase_parser_rejects_failure_and_accepts_exact_duration() {
        let good = "checkpoint_phase_ledger\tattempt=3\tbudget_us=0\tclock=ok\tcomponent=native\tduration_us=17\tgeneration=3\tisa=x86_64\toutcome=progress\tphase=serialization\tsession=3\tstatus=0";
        let parsed = rows(good).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].duration_us, 17);
        assert!(rows(&good.replace("progress", "failure")).is_err());
        assert!(rows(&good.replace("duration_us=17\t", "")).is_err());
    }

    #[test]
    fn phase_validator_requires_every_phase_in_order_for_both_isas() {
        let mut rows = Vec::new();
        for isa in ["aarch64", "x86_64"] {
            for phase in NATIVE {
                rows.push(super::Row {
                    component: "native".into(),
                    isa: isa.into(),
                    phase: phase.into(),
                    duration_us: 1,
                });
            }
            for phase in CONTROL {
                rows.push(super::Row {
                    component: "control".into(),
                    isa: isa.into(),
                    phase: phase.into(),
                    duration_us: 1,
                });
            }
        }
        validate(&rows).unwrap();
        rows.remove(0);
        assert!(validate(&rows).is_err());
    }
}
