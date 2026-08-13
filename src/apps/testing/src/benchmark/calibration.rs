use super::{definition::Campaign, evidence, evidence::Measurement, ledger::Ledger, schedule};
use crate::suite::Error;
use clap::Args;
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Args)]
pub(crate) struct Options {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    results: PathBuf,
    /// Comma-delimited subset of E,I,R to calibrate independently.
    #[arg(long, value_delimiter = ',', required = true)]
    arms: Vec<String>,
    #[arg(long, default_value_t = 12)]
    rounds: u32,
    #[arg(long)]
    resume: bool,
    #[arg(long, default_value_t = 30.0)]
    minimum_free_gib: f64,
    #[arg(long, default_value_t = 120)]
    quiet_seconds: u64,
    #[arg(long, default_value_t = 900)]
    lock_timeout: u64,
    #[arg(long, default_value_t = 1.0)]
    max_load: f64,
}

pub(super) fn run(options: Options) -> Result<(), Error> {
    validate(&options.arms, options.rounds)?;
    let workspace = crate::runtime::workspace()?;
    let campaign = Campaign::load(&workspace.join(&options.config))?;
    if options.arms.iter().any(|arm| !campaign.arms.contains_key(arm)) {
        return Err("calibration arm is absent from campaign".into());
    }
    campaign.verify_artifacts()?;
    let plan = schedule::calibration(&campaign, &options.arms, options.rounds);
    if plan.is_empty() {
        return Err("calibration has no compatible measurements".into());
    }
    let result_path = workspace.join(&options.results);
    if options.resume && result_path.join("qualification.txt").exists() {
        return Err("calibration result directory is already published; use a unique path for a new run".into());
    }
    let mode = format!("calibration:{}:{}", options.arms.join(","), options.rounds);
    let mut ledger = Ledger::open_planned(&result_path, &campaign, options.resume, plan.clone(), &mode)?;
    ledger.require_space(options.minimum_free_gib)?;
    let _measurement = Measurement::acquire(options.quiet_seconds, options.lock_timeout, options.max_load)?;
    for step in schedule::warmups(&campaign)
        .into_iter()
        .filter(|step| options.arms.contains(&step.arm))
    {
        evidence::sample(&campaign, &step)?;
    }
    for pair in plan.chunks_exact(2) {
        let [first, second] = pair else { unreachable!() };
        let present = [ledger.contains(&first.key()), ledger.contains(&second.key())];
        if present == [true, true] {
            continue;
        }
        if present != [false, false] {
            return Err("calibration ledger contains an incomplete pair".into());
        }
        ledger.append(&evidence::measure(&campaign, first)?)?;
        ledger.append(&evidence::measure(&campaign, second)?)?;
    }
    campaign.verify_artifacts()?;
    let report = report(&campaign, &options.arms, &ledger.complete()?, options.rounds)?;
    fs::write(result_path.join("calibration.tsv"), &report.text)?;
    fs::write(result_path.join("qualification.txt"), format!("{}\n", report.status))?;
    print!("{}", report.text);
    println!("CALIBRATION\t{}\tidentity={}", report.status, campaign.identity()?);
    if report.status == "QUALIFIED" {
        Ok(())
    } else {
        Err("benchmark calibration is unqualified".into())
    }
}

fn validate(arms: &[String], rounds: u32) -> Result<(), Error> {
    if rounds < 12 || !rounds.is_multiple_of(4) {
        return Err("calibration rounds must be at least 12 and a multiple of four".into());
    }
    let mut sorted = arms.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.is_empty()
        || sorted.len() != arms.len()
        || sorted.iter().any(|arm| !matches!(arm.as_str(), "E" | "I" | "R"))
    {
        return Err("calibration arms must be a unique subset of E,I,R".into());
    }
    Ok(())
}

struct Report {
    status: &'static str,
    text: String,
}

fn report(campaign: &Campaign, requested: &[String], rows: &[evidence::Row], rounds: u32) -> Result<Report, Error> {
    for arm in requested {
        if !rows.iter().any(|row| &row.arm == arm) {
            return Err(format!("requested calibration arm {arm} has no measured compatible context").into());
        }
    }
    let mut outputs = BTreeMap::<(&str, &str, &str), (&str, &str, Option<&str>)>::new();
    for row in rows {
        let observed = (
            row.output.as_str(),
            row.output_frame.as_str(),
            row.diagnostic.as_deref(),
        );
        let expected = outputs
            .entry((&row.arm, &row.workload, &row.layout))
            .or_insert(observed);
        if *expected != observed {
            return Err("same-arm calibration exact output changed between pairs".into());
        }
    }
    let mut groups = BTreeMap::<(&str, &str, &str, &str), BTreeMap<u32, [&evidence::Row; 2]>>::new();
    for row in rows {
        for phase in row.phases.keys() {
            let rounds = groups.entry((&row.arm, &row.workload, &row.layout, phase)).or_default();
            let pair = rounds.entry(row.round).or_insert([row, row]);
            pair[row.position] = row;
        }
    }
    let mut qualified = true;
    let mut lines = vec!["arm\tworkload\tlayout\tphase\tcenter\torder_ab\torder_ba\ttemporal_early\ttemporal_late\tmax_deviation\tstatus".into()];
    for arm in requested {
        for (workload, definition) in &campaign.workloads {
            for (layout, support) in &definition.arm_support {
                if let super::definition::ArmSupport::Incompatible {
                    status,
                    stderr,
                    artifact_sha256,
                } = &support[arm]
                {
                    lines.push(format!(
                        "{arm}\t{workload}\t{layout}\t-\t-\t-\t-\t-\t-\t-\tOMITTED classified-incompatible status={status} artifact={artifact_sha256} stderr={}",
                        bounded(stderr)
                    ));
                }
            }
        }
    }
    for ((arm, workload, layout, phase), by_round) in groups {
        if by_round.len() != rounds as usize {
            return Err("calibration evidence is incomplete".into());
        }
        let values = by_round
            .values()
            .map(|pair| pair[1].phases[phase].us as f64 / pair[0].phases[phase].us.max(1) as f64)
            .collect::<Vec<_>>();
        let center = median(&values);
        let ab = median(
            &values
                .iter()
                .enumerate()
                .filter(|(i, _)| matches!(i % 4, 0 | 3))
                .map(|(_, v)| *v)
                .collect::<Vec<_>>(),
        );
        let ba = median(
            &values
                .iter()
                .enumerate()
                .filter(|(i, _)| matches!(i % 4, 1 | 2))
                .map(|(_, v)| *v)
                .collect::<Vec<_>>(),
        );
        let middle = values.len() / 2;
        let early = median(&values[..middle]);
        let late = median(&values[middle..]);
        let maximum = values.iter().map(|value| (value - 1.0).abs()).fold(0.0_f64, f64::max);
        let invariant = campaign.invariant_phases.iter().any(|declared| declared == phase);
        let ok = qualifies([center, ab, ba, early, late], maximum, invariant);
        qualified &= ok;
        lines.push(format!("{arm}\t{workload}\t{layout}\t{phase}\t{center:.6}\t{ab:.6}\t{ba:.6}\t{early:.6}\t{late:.6}\t{maximum:.6}\t{}", if ok { "QUALIFIED" } else { "UNQUALIFIED" }));
    }
    Ok(Report {
        status: if qualified { "QUALIFIED" } else { "UNQUALIFIED" },
        text: lines.join("\n") + "\n",
    })
}

fn qualifies(strata: [f64; 5], maximum: f64, invariant: bool) -> bool {
    strata.into_iter().all(|value| (value - 1.0).abs() <= 0.01)
        && maximum <= 0.05
        && maximum <= if invariant { 0.015 } else { 0.03 }
}

fn bounded(value: &str) -> String {
    const LIMIT: usize = 160;
    let value = value.replace(['\r', '\n', '\t'], " ");
    if value.len() <= LIMIT {
        value
    } else {
        let mut end = LIMIT;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{} [truncated]", &value[..end])
    }
}

fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn options_reject_short_rounds_and_duplicate_or_unknown_arms() {
        assert!(super::validate(&["E".into(), "R".into()], 12).is_ok());
        assert!(super::validate(&["E".into()], 8).is_err());
        assert!(super::validate(&["E".into(), "E".into()], 12).is_err());
        assert!(super::validate(&["X".into()], 12).is_err());
    }

    #[test]
    fn order_strata_are_zero_three_against_one_two() {
        let values = [1.0, 0.9, 0.9, 1.0];
        let ab = super::median(
            &values
                .iter()
                .enumerate()
                .filter(|(i, _)| matches!(i % 4, 0 | 3))
                .map(|(_, v)| *v)
                .collect::<Vec<_>>(),
        );
        let ba = super::median(
            &values
                .iter()
                .enumerate()
                .filter(|(i, _)| matches!(i % 4, 1 | 2))
                .map(|(_, v)| *v)
                .collect::<Vec<_>>(),
        );
        assert_eq!((ab, ba), (1.0, 0.9));
    }

    #[test]
    fn incompatible_diagnostic_is_single_line_and_bounded() {
        let value = super::bounded(&format!("failure\n{}", "x".repeat(256)));
        assert!(!value.contains('\n'));
        assert!(value.ends_with(" [truncated]"));
        assert!(value.len() <= 172);
    }

    #[test]
    fn qualification_uses_acceptance_null_floors() {
        assert!(super::qualifies([1.0; 5], 0.015, true));
        assert!(!super::qualifies([1.0; 5], 0.015_1, true));
        assert!(super::qualifies([1.0; 5], 0.03, false));
        assert!(!super::qualifies([1.0; 5], 0.030_1, false));
    }
}
