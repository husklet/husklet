use super::{definition::Campaign, evidence::Row, schedule::CELLS};
use crate::suite::Error;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Report {
    pub verdict: &'static str,
    pub text: String,
}

pub(super) fn evaluate(campaign: &Campaign, rows: &[Row], limit: f64) -> Result<Report, Error> {
    if !limit.is_finite() || limit < 1.0 {
        return Err("benchmark limit must be finite and at least 1".into());
    }
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
    Ok(Report {
        verdict,
        text: lines.join("\n") + "\n",
    })
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
        let floor = nulls[&(workload, layout, left, phase_name)].max(nulls[&(workload, layout, right, phase_name)]);
        let upper = ratio * (1.0 + floor);
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
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, Row};

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
}
