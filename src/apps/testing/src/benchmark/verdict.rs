use super::{
    definition::Campaign,
    evidence::Row,
    schedule::{self, CELLS, Step},
};
use crate::suite::Error;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Report {
    pub verdict: &'static str,
    pub text: String,
}

impl Report {
    pub(super) fn evaluate(campaign: &Campaign, rows: &[Row], limit: f64) -> Result<Self, Error> {
        if !limit.is_finite() || limit < 1.0 {
            return Err("benchmark limit must be finite and at least 1".into());
        }
        let plan = schedule::measurements(campaign);
        verify_balanced_order(&plan)?;
        verify_plan(campaign, &plan, rows)?;
        verify_outputs(rows)?;
        let by_key = rows
            .iter()
            .map(|row| (row.key.as_str(), row))
            .collect::<BTreeMap<_, _>>();
        let mut nulls = BTreeMap::new();
        let invariant = campaign
            .invariant_phases
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for (workload, definition) in &campaign.workloads {
            collect_nulls(campaign, &by_key, &invariant, workload, definition, &mut nulls)?;
        }
        let mut verdict = "PASS";
        let mut lines = vec!["workload\tlayout\tcell\tphase\tratio\tnull_floor\tupper\tverdict".to_owned()];
        for (workload, definition) in &campaign.workloads {
            if append_comparisons(campaign, &by_key, &nulls, workload, definition, limit, &mut lines)? {
                verdict = "FAIL";
            }
        }
        lines.push("artifact\tsha256".to_owned());
        for (arm, definition) in &campaign.arms {
            for (name, artifact) in &definition.artifacts {
                lines.push(format!("{arm}/{name}\t{}", artifact.sha256));
            }
        }
        lines.push(format!("rootfs\t{}", campaign.rootfs.sha256));
        lines.push("sample\thost_load".to_owned());
        for row in rows {
            lines.push(format!("{}\t{}", row.key, row.host_load));
        }
        Ok(Self {
            verdict,
            text: lines.join("\n") + "\n",
        })
    }
}

fn verify_balanced_order(plan: &[Step]) -> Result<(), Error> {
    let mut cells = BTreeMap::<(&str, &str, &str), Vec<&Step>>::new();
    for step in plan {
        if step.cell.len() != 2 {
            return Err("benchmark schedule has an invalid cell".into());
        }
        let (left, right) = step.cell.split_at(1);
        if left != right {
            cells
                .entry((&step.workload, &step.layout, &step.cell))
                .or_default()
                .push(step);
        }
    }
    for ((workload, layout, cell), steps) in cells {
        if !steps.len().is_multiple_of(8) {
            return Err(format!("benchmark schedule is not four-round balanced for {workload}/{layout}/{cell}").into());
        }
        for block in steps.chunks_exact(8) {
            let first = block
                .chunks_exact(2)
                .map(|pair| {
                    if pair[0].round != pair[1].round
                        || pair[0].position != 0
                        || pair[1].position != 1
                        || pair[0].arm == pair[1].arm
                    {
                        return Err("benchmark pair does not contain both arms in two positions");
                    }
                    Ok(pair[0].arm.as_str())
                })
                .collect::<Result<Vec<_>, _>>()?;
            if first[0] == first[1] || first[2] == first[3] || first[0] != first[3] || first[1] != first[2] {
                return Err(
                    format!("benchmark schedule has unbalanced order strata for {workload}/{layout}/{cell}").into(),
                );
            }
        }
    }
    Ok(())
}

fn verify_plan(campaign: &Campaign, expected: &[Step], rows: &[Row]) -> Result<(), Error> {
    if expected.len() != rows.len() {
        return Err(format!(
            "benchmark evidence cardinality differs from plan: expected {}, observed {}",
            expected.len(),
            rows.len()
        )
        .into());
    }
    let mut observed = BTreeMap::new();
    for row in rows {
        if observed.insert(row.key.as_str(), row).is_some() {
            return Err(format!("duplicate benchmark evidence key {}", row.key).into());
        }
    }
    for step in expected {
        let key = step.key();
        let row = observed
            .get(key.as_str())
            .ok_or_else(|| format!("missing benchmark evidence key {key}"))?;
        verify_row_provenance(step, row)?;
        verify_phase_coverage(row, phases(campaign, &step.workload, &step.layout))?;
        verify_host_load(row)?;
    }
    Ok(())
}

fn verify_host_load(row: &Row) -> Result<(), Error> {
    let load = row
        .host_load
        .parse::<f64>()
        .map_err(|_| format!("benchmark evidence has invalid host load for {}", row.key))?;
    if !load.is_finite() || load < 0.0 {
        return Err(format!("benchmark evidence has invalid host load for {}", row.key).into());
    }
    Ok(())
}

fn verify_row_provenance(step: &Step, row: &Row) -> Result<(), Error> {
    if row.workload != step.workload
        || row.layout != step.layout
        || row.cell != step.cell
        || row.round != step.round
        || row.position != step.position
        || row.arm != step.arm
    {
        Err(format!("benchmark evidence provenance differs from plan for {}", step.key()).into())
    } else {
        Ok(())
    }
}

fn verify_phase_coverage(row: &Row, expected: &[String]) -> Result<(), Error> {
    let observed = row.phases.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(format!(
            "benchmark evidence phase coverage differs from campaign for {}/{}",
            row.workload, row.layout
        )
        .into());
    }
    if row.phases.values().any(|phase| phase.us == 0) {
        return Err(format!(
            "benchmark evidence contains a zero duration for {}/{}",
            row.workload, row.layout
        )
        .into());
    }
    Ok(())
}

type NullKey<'a> = (&'a str, &'a str, &'a str, &'a str);

fn phases<'a>(campaign: &'a Campaign, workload: &str, layout: &str) -> &'a [String] {
    if workload == "python" {
        &campaign.workloads[workload].phases
    } else {
        &campaign.layouts[layout].phases
    }
}

fn collect_nulls<'a>(
    campaign: &'a Campaign,
    rows: &BTreeMap<&str, &Row>,
    invariant: &BTreeSet<&str>,
    workload: &'a str,
    definition: &'a super::definition::Workload,
    nulls: &mut BTreeMap<NullKey<'a>, f64>,
) -> Result<(), Error> {
    for layout in definition.commands.keys() {
        collect_layout_nulls(campaign, rows, invariant, workload, layout, nulls)?;
    }
    Ok(())
}

fn collect_layout_nulls<'a>(
    campaign: &'a Campaign,
    rows: &BTreeMap<&str, &Row>,
    invariant: &BTreeSet<&str>,
    workload: &'a str,
    layout: &'a str,
    nulls: &mut BTreeMap<NullKey<'a>, f64>,
) -> Result<(), Error> {
    for arm in ["E", "R", "I"] {
        let cell = format!("{arm}{arm}");
        for phase in phases(campaign, workload, layout) {
            let ratios = paired(rows, workload, layout, &cell, arm, arm, phase, campaign.rounds)?;
            nulls.insert(
                (workload, layout, arm, phase),
                qualify_null(&ratios, invariant.contains(phase.as_str()))?,
            );
        }
    }
    Ok(())
}

fn append_comparisons(
    campaign: &Campaign,
    rows: &BTreeMap<&str, &Row>,
    nulls: &BTreeMap<NullKey<'_>, f64>,
    workload: &str,
    definition: &super::definition::Workload,
    limit: f64,
    lines: &mut Vec<String>,
) -> Result<bool, Error> {
    let mut failed = false;
    for layout in definition.commands.keys() {
        for &(left, right) in &CELLS[3..] {
            failed |= append_cell(campaign, rows, nulls, workload, layout, left, right, limit, lines)?;
        }
    }
    Ok(failed)
}

fn append_cell(
    campaign: &Campaign,
    rows: &BTreeMap<&str, &Row>,
    nulls: &BTreeMap<NullKey<'_>, f64>,
    workload: &str,
    layout: &str,
    left: &str,
    right: &str,
    limit: f64,
    lines: &mut Vec<String>,
) -> Result<bool, Error> {
    let cell = format!("{left}{right}");
    let mut failed = false;
    for phase in phases(campaign, workload, layout) {
        let mut ratios = paired(rows, workload, layout, &cell, left, right, phase, campaign.rounds)?;
        let ratio = median(&mut ratios);
        let phase_name = phase.as_str();
        let left_floor = nulls[&(workload, layout, left, phase_name)];
        let right_floor = nulls[&(workload, layout, right, phase_name)];
        let floor = left_floor.max(right_floor);
        let upper = corrected_upper(ratio, left_floor, right_floor)?;
        let judged = right == "I" && campaign.workloads[workload].phases.iter().any(|item| item == phase);
        let result = if judged && upper > limit {
            failed = true;
            "FAIL"
        } else if judged {
            "PASS"
        } else {
            "INFO"
        };
        lines.push(format!(
            "{workload}\t{layout}\t{cell}\t{phase}\t{ratio:.6}\t{floor:.6}\t{upper:.6}\t{result}"
        ));
    }
    Ok(failed)
}

fn corrected_upper(ratio: f64, left_floor: f64, right_floor: f64) -> Result<f64, Error> {
    if !ratio.is_finite()
        || ratio < 0.0
        || !left_floor.is_finite()
        || !(0.0..1.0).contains(&left_floor)
        || !right_floor.is_finite()
        || !(0.0..1.0).contains(&right_floor)
    {
        return Err("invalid ratio or null floor in benchmark correction".into());
    }
    // The crossed ratio is smallest when the left sample is high by its null floor and
    // the right sample is low by its null floor. Undo both directions before judging it.
    Ok(ratio * (1.0 + left_floor) / (1.0 - right_floor))
}

fn verify_outputs(rows: &[Row]) -> Result<(), Error> {
    let mut expected = BTreeMap::new();
    for row in rows {
        let value = (
            row.output.as_str(),
            row.phases
                .iter()
                .map(|(name, phase)| (name.as_str(), phase.ok.as_str()))
                .collect::<Vec<_>>(),
        );
        let key = (row.workload.as_str(), row.layout.as_str());
        if expected.insert(key, value.clone()).is_some_and(|prior| prior != value) {
            return Err(format!("exact-output mismatch for {}/{}", row.workload, row.layout).into());
        }
    }
    Ok(())
}

fn paired(
    by_key: &BTreeMap<&str, &Row>,
    workload: &str,
    layout: &str,
    cell: &str,
    left: &str,
    right: &str,
    phase: &str,
    rounds: u32,
) -> Result<Vec<f64>, Error> {
    let mut values = Vec::new();
    for round in 0..rounds {
        let first = by_key
            .get(format!("{workload}|{layout}|{cell}|{round}|0").as_str())
            .ok_or("missing first paired row")?;
        let second = by_key
            .get(format!("{workload}|{layout}|{cell}|{round}|1").as_str())
            .ok_or("missing second paired row")?;
        let (a, b) = if left == right {
            (
                first.phases.get(phase).ok_or("pair omitted phase")?.us,
                second.phases.get(phase).ok_or("pair omitted phase")?.us,
            )
        } else {
            let samples = [(first.arm.as_str(), first), (second.arm.as_str(), second)]
                .into_iter()
                .collect::<BTreeMap<_, _>>();
            (
                samples
                    .get(left)
                    .ok_or("pair omitted left arm")?
                    .phases
                    .get(phase)
                    .ok_or("pair omitted phase")?
                    .us,
                samples
                    .get(right)
                    .ok_or("pair omitted right arm")?
                    .phases
                    .get(phase)
                    .ok_or("pair omitted phase")?
                    .us,
            )
        };
        values.push(b as f64 / a.max(1) as f64);
    }
    Ok(values)
}

fn qualify_null(values: &[f64], invariant: bool) -> Result<f64, Error> {
    if values.len() < 4 {
        return Err("null has fewer than four paired samples".into());
    }
    let mut all = values.to_vec();
    let center = median(&mut all);
    let mut even = values.iter().step_by(2).copied().collect::<Vec<_>>();
    let mut odd = values.iter().skip(1).step_by(2).copied().collect::<Vec<_>>();
    let middle = values.len() / 2;
    let mut early = values[..middle].to_vec();
    let mut late = values[middle..].to_vec();
    let floor = values.iter().map(|value| (value - 1.0).abs()).fold(0.0_f64, f64::max);
    if (center - 1.0).abs() > 0.01 {
        return Err("unqualified null: center".into());
    }
    if [median(&mut even), median(&mut odd)]
        .into_iter()
        .any(|value| (value - 1.0).abs() > 0.01)
    {
        return Err("unqualified null: order strata".into());
    }
    if [median(&mut early), median(&mut late)]
        .into_iter()
        .any(|value| (value - 1.0).abs() > 0.01)
    {
        return Err("unqualified null: temporal strata".into());
    }
    if values.iter().any(|value| (value - 1.0).abs() > 0.05) {
        return Err("unqualified null: individual pair".into());
    }
    if floor > if invariant { 0.015 } else { 0.03 } {
        return Err(format!(
            "unqualified null: {} floor",
            if invariant { "invariant" } else { "null" }
        )
        .into());
    }
    Ok(floor)
}

fn median(values: &mut [f64]) -> f64 {
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
    use super::{BTreeMap, Row, Step};

    fn row(key: &str, arm: &str, us: u64) -> Row {
        Row {
            key: key.into(),
            workload: "malloc".into(),
            layout: "plain".into(),
            cell: "EE".into(),
            round: 0,
            position: 0,
            arm: arm.into(),
            output: "same".into(),
            phases: [("malloc".into(), super::super::evidence::Phase { us, ok: "same".into() })].into(),
            host_load: "0.1".into(),
        }
    }

    #[test]
    fn null_qualification_is_fail_closed() {
        assert!((super::qualify_null(&[1.004, 0.997, 1.003, 0.998], false).unwrap() - 0.004).abs() < 1e-9);
        assert!(super::qualify_null(&[1.02; 4], false).is_err());
        assert!(super::qualify_null(&[1.051, 0.983, 0.983, 0.983], false).is_err());
    }

    #[test]
    fn control_correction_accounts_for_drift_in_both_arms() {
        let upper = super::corrected_upper(1.07, 0.02, 0.02).unwrap();
        assert!(upper > 1.10);
        assert!(1.07 * 1.02 < 1.10, "one-sided correction would falsely pass");
        assert!(super::corrected_upper(1.0, 0.0, 1.0).is_err());
    }

    #[test]
    fn same_arm_null_compares_first_and_second_positions() {
        let first = row("malloc|plain|EE|0|0", "E", 100);
        let second = row("malloc|plain|EE|0|1", "E", 120);
        let rows = [first, second];
        let by_key = rows
            .iter()
            .map(|row| (row.key.as_str(), row))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            super::paired(&by_key, "malloc", "plain", "EE", "E", "E", "malloc", 1).unwrap(),
            [1.2]
        );
    }

    #[test]
    fn evidence_provenance_must_match_the_scheduled_binary_and_layout() {
        let step = Step {
            workload: "malloc".into(),
            layout: "plain".into(),
            cell: "EE".into(),
            round: 0,
            position: 0,
            arm: "E".into(),
        };
        let valid = row("malloc|plain|EE|0|0", "E", 100);
        super::verify_row_provenance(&step, &valid).unwrap();

        let mut wrong_binary = valid.clone();
        wrong_binary.arm = "R".into();
        assert!(super::verify_row_provenance(&step, &wrong_binary).is_err());

        let mut wrong_layout = valid.clone();
        wrong_layout.layout = "sqlite".into();
        assert!(super::verify_row_provenance(&step, &wrong_layout).is_err());
    }

    #[test]
    fn evidence_phase_coverage_must_be_exact() {
        let expected = ["malloc".to_owned()];
        let complete = row("malloc|plain|EE|0|0", "E", 100);
        super::verify_phase_coverage(&complete, &expected).unwrap();

        let mut missing = complete.clone();
        missing.phases.clear();
        assert!(super::verify_phase_coverage(&missing, &expected).is_err());

        let mut extra = complete;
        extra.phases.insert(
            "unplanned".into(),
            super::super::evidence::Phase {
                us: 1,
                ok: "same".into(),
            },
        );
        assert!(super::verify_phase_coverage(&extra, &expected).is_err());
    }

    #[test]
    fn evidence_duration_must_be_positive() {
        let expected = ["malloc".to_owned()];
        let mut zero = row("malloc|plain|EE|0|0", "E", 0);
        assert!(super::verify_phase_coverage(&zero, &expected).is_err());
        zero.phases.get_mut("malloc").unwrap().us = 1;
        super::verify_phase_coverage(&zero, &expected).unwrap();
    }

    #[test]
    fn host_load_must_be_finite_numeric_evidence() {
        let mut evidence = row("malloc|plain|EE|0|0", "E", 1);
        for invalid in ["unavailable", "NaN", "inf", "-0.1", ""] {
            evidence.host_load = invalid.into();
            assert!(super::verify_host_load(&evidence).is_err(), "accepted {invalid:?}");
        }
        evidence.host_load = "0.25".into();
        super::verify_host_load(&evidence).unwrap();
    }

    #[test]
    fn crossed_schedule_balance_is_independently_validated() {
        let steps = |first: [&str; 4]| {
            first
                .into_iter()
                .enumerate()
                .flat_map(|(round, first)| {
                    let second = if first == "E" { "I" } else { "E" };
                    [first, second]
                        .into_iter()
                        .enumerate()
                        .map(move |(position, arm)| Step {
                            workload: "malloc".into(),
                            layout: "plain".into(),
                            cell: "EI".into(),
                            round: round as u32,
                            position,
                            arm: arm.into(),
                        })
                })
                .collect::<Vec<_>>()
        };
        super::verify_balanced_order(&steps(["E", "I", "I", "E"])).unwrap();
        assert!(super::verify_balanced_order(&steps(["E", "E", "E", "E"])).is_err());
        assert!(super::verify_balanced_order(&steps(["E", "I", "E", "I"])).is_err());
    }
}
