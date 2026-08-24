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
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u32).range(1..=2048))]
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

#[derive(Debug)]
struct FdScan {
    isa: String,
    pass: u32,
    visible: u64,
    captured: u64,
    comparisons: u64,
    hash: String,
}

fn fd_scans(stderr: &str) -> Result<Vec<FdScan>, String> {
    stderr
        .lines()
        .filter(|line| line.starts_with("checkpoint_fd_scan\t"))
        .map(|line| {
            let fields = line
                .split('\t')
                .skip(1)
                .filter_map(|field| field.split_once('='))
                .collect::<BTreeMap<_, _>>();
            if fields.keys().copied().collect::<Vec<_>>()
                != ["captured", "comparisons", "hash", "isa", "pass", "visible"]
            {
                return Err(format!("fd scan schema mismatch: {line}"));
            }
            let number = |name: &str| {
                fields[name]
                    .parse::<u64>()
                    .map_err(|_| format!("invalid fd scan {name}: {line}"))
            };
            let pass = fields["pass"]
                .parse::<u32>()
                .map_err(|_| format!("invalid fd scan pass: {line}"))?;
            let hash = fields["hash"];
            if hash.len() != 16
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(format!("invalid fd scan identity/class hash: {line}"));
            }
            Ok(FdScan {
                isa: fields["isa"].to_owned(),
                pass,
                visible: number("visible")?,
                captured: number("captured")?,
                comparisons: number("comparisons")?,
                hash: hash.to_owned(),
            })
        })
        .collect()
}

fn validate_fd_scans(scans: &[FdScan]) -> Result<(), String> {
    for isa in ["aarch64", "x86_64"] {
        let rows = scans.iter().filter(|row| row.isa == isa).collect::<Vec<_>>();
        if rows.len() != 12 {
            return Err(format!("{isa} expected 12 fd scan rows, got {}", rows.len()));
        }
        let signatures = |pass| {
            let mut values = rows
                .iter()
                .filter(|row| row.pass == pass)
                .map(|row| (row.visible, row.captured, row.comparisons, row.hash.as_str()))
                .collect::<Vec<_>>();
            values.sort_unstable();
            values
        };
        let admission = signatures(1);
        let consumption = signatures(2);
        if admission.len() != 6 || admission != consumption {
            return Err(format!(
                "{isa} admission/consumption descriptor sets differ: admission={admission:?} consumption={consumption:?}"
            ));
        }
        if rows.iter().any(|row| row.comparisons != 0) {
            return Err(format!(
                "{isa} descriptor scan performed redundant prior-record comparisons"
            ));
        }
    }
    if scans
        .iter()
        .any(|row| !matches!(row.isa.as_str(), "aarch64" | "x86_64"))
    {
        return Err("unknown fd scan ISA".to_owned());
    }
    Ok(())
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
    let mut scan_totals = BTreeMap::<String, (u64, u64, u64, u64)>::new();
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
        let scans =
            fd_scans(std::str::from_utf8(&output.stderr)?).map_err(|error| format!("sample {sample}: {error}"))?;
        validate_fd_scans(&scans).map_err(|error| format!("sample {sample}: {error}"))?;
        for scan in scans {
            let total = scan_totals.entry(scan.isa).or_default();
            total.0 += scan.visible;
            total.1 += scan.captured;
            total.2 += scan.comparisons;
            total.3 += 1;
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
            "checkpoint_profile_v1\nsamples={}\nscale={}\nprobe_sha256={probe_hash}\nfd_scan_rows_per_sample=24\nfd_scan_admission_equals_consumption=1\n",
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
    for (isa, (visible, captured, comparisons, count)) in scan_totals {
        println!(
            "checkpoint_profile_fd_scan\tisa={isa}\trows={count}\tmean_visible={}\tmean_captured={}\tmean_comparisons={}",
            visible / count,
            captured / count,
            comparisons / count
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CONTROL, NATIVE, fd_scans, rows, validate, validate_fd_scans};

    #[test]
    fn fd_scan_receipt_requires_identical_admission_and_consumption_sets() {
        let pair = |isa: &str| {
            format!(
                "checkpoint_fd_scan\tisa={isa}\tpass=1\tvisible=7\tcaptured=7\tcomparisons=0\thash=0123456789abcdef\ncheckpoint_fd_scan\tisa={isa}\tpass=2\tvisible=7\tcaptured=7\tcomparisons=0\thash=0123456789abcdef\n"
            )
        };
        let mut text = String::new();
        for isa in ["aarch64", "x86_64"] {
            for _ in 0..6 {
                text.push_str(&pair(isa));
            }
        }
        let rows = fd_scans(&text).unwrap();
        validate_fd_scans(&rows).unwrap();
        assert!(
            validate_fd_scans(&fd_scans(&text.replacen("hash=0123456789abcdef", "hash=fedcba9876543210", 1)).unwrap())
                .is_err()
        );
        assert!(fd_scans(&text.replacen("pass=1", "pass=4294967297", 1)).is_err());
        assert!(fd_scans(&text.replacen("0123456789abcdef", "0123456789abcde", 1)).is_err());
        assert!(fd_scans(&text.replacen("0123456789abcdef", "0123456789abcdeF", 1)).is_err());
    }

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
