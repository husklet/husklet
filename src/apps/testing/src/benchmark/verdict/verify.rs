use super::{BTreeMap, BTreeSet, CELLS, Campaign, Error, ProfileKind, Row, Step};

pub(super) fn verify_complete_plan(campaign: &Campaign, plan: &[Step]) -> Result<(), Error> {
    let expected = campaign
        .workloads
        .iter()
        .flat_map(|(workload, definition)| {
            definition.commands.keys().flat_map(move |layout| {
                let support = &definition.arm_support[layout];
                let crossed = CELLS
                    .into_iter()
                    .filter(move |(left, right)| support[*left].available() && support[*right].available())
                    .flat_map(move |(left, right)| {
                        (0..campaign.rounds).flat_map(move |round| {
                            [left, right].into_iter().map(move |arm| {
                                (
                                    workload.as_str(),
                                    layout.as_str(),
                                    format!("{left}{right}"),
                                    round,
                                    arm,
                                    ProfileKind::Primary,
                                )
                            })
                        })
                    });
                let nulls = ["E", "I"].into_iter().flat_map(move |arm| {
                    (0..campaign.rounds).flat_map(move |round| {
                        [ProfileKind::Primary, ProfileKind::IndependentNull]
                            .into_iter()
                            .map(move |profile| {
                                (
                                    workload.as_str(),
                                    layout.as_str(),
                                    format!("{arm}{arm}"),
                                    round,
                                    arm,
                                    profile,
                                )
                            })
                    })
                });
                crossed.chain(nulls)
            })
        })
        .fold(BTreeMap::new(), |mut counts, key| {
            *counts.entry(key).or_insert(0_u8) += 1;
            counts
        });
    verify_expected_plan(expected, plan)
}

pub(super) fn verify_expected_plan(
    expected: BTreeMap<(&str, &str, String, u32, &str, ProfileKind), u8>,
    plan: &[Step],
) -> Result<(), Error> {
    let observed = plan.iter().fold(BTreeMap::new(), |mut counts, step| {
        *counts
            .entry((
                step.workload.as_str(),
                step.layout.as_str(),
                step.cell.clone(),
                step.round,
                step.arm.as_str(),
                step.profile,
            ))
            .or_insert(0_u8) += 1;
        counts
    });
    if observed != expected {
        return Err("benchmark schedule does not cover every compatible campaign arm, layout, workload, cell, and round exactly once".into());
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn verify_context_plan(contexts: &[(&str, &str)], rounds: u32, plan: &[Step]) -> Result<(), Error> {
    let expected = contexts
        .iter()
        .flat_map(|&(workload, layout)| {
            CELLS.into_iter().flat_map(move |(left, right)| {
                (0..rounds).flat_map(move |round| {
                    [left, right].into_iter().map(move |arm| {
                        (
                            workload,
                            layout,
                            format!("{left}{right}"),
                            round,
                            arm,
                            ProfileKind::Primary,
                        )
                    })
                })
            })
        })
        .fold(BTreeMap::new(), |mut counts, key| {
            *counts.entry(key).or_insert(0_u8) += 1;
            counts
        });
    verify_expected_plan(expected, plan)
}

pub(super) fn verify_balanced_order(plan: &[Step]) -> Result<(), Error> {
    let mut cells = BTreeMap::<(&str, &str, &str), Vec<&Step>>::new();
    for step in plan {
        if step.cell.len() != 2 {
            return Err("benchmark schedule has an invalid cell".into());
        }
        cells
            .entry((&step.workload, &step.layout, &step.cell))
            .or_default()
            .push(step);
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
                        || (pair[0].arm == pair[1].arm && pair[0].profile == pair[1].profile)
                    {
                        return Err("benchmark pair does not contain both arms in two positions");
                    }
                    Ok((pair[0].arm.as_str(), pair[0].profile))
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

pub(super) fn verify_plan(campaign: &Campaign, expected: &[Step], rows: &[Row]) -> Result<(), Error> {
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
        verify_phase_frame(row)?;
        verify_host_load(row, campaign.samples_per_row)?;
    }
    Ok(())
}

#[hl_design::classify(domain = "benchmark evidence")]
pub(super) fn verify_phase_frame(row: &Row) -> Result<(), Error> {
    let framed = row
        .output_frame
        .lines()
        .filter(|line| line.starts_with("PHASE "))
        .collect::<BTreeSet<_>>();
    let expected = row
        .phases
        .iter()
        .map(|(name, phase)| format!("PHASE {name} us=<time> ok={}", phase.ok))
        .collect::<BTreeSet<_>>();
    if framed.len() != row.phases.len() || expected.iter().any(|line| !framed.contains(line.as_str())) {
        return Err(format!(
            "benchmark phase evidence differs from exact-output frame for {}",
            row.key
        )
        .into());
    }
    Ok(())
}

pub(super) fn verify_host_load(row: &Row, samples: u32) -> Result<(), Error> {
    if !row.host_load_valid(samples) {
        return Err(format!("benchmark evidence has invalid host load for {}", row.key).into());
    }
    Ok(())
}

pub(super) fn verify_row_provenance(step: &Step, row: &Row) -> Result<(), Error> {
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

pub(super) fn verify_phase_coverage(row: &Row, expected: &[String]) -> Result<(), Error> {
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

pub(super) type NullKey<'a> = (&'a str, &'a str, &'a str, &'a str);

pub(super) fn phases<'a>(campaign: &'a Campaign, workload: &str, layout: &str) -> &'a [String] {
    &campaign.workloads[workload].layout_phases[layout]
}
