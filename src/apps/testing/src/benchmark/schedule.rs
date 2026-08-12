use super::definition::Campaign;

pub(super) const CELLS: [(&str, &str); 6] = [("E", "E"), ("R", "R"), ("I", "I"), ("E", "R"), ("E", "I"), ("R", "I")];
const ORDER: [[usize; 2]; 4] = [[0, 1], [1, 0], [1, 0], [0, 1]];
const WARMUP_PAIRS: u32 = 4;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Step {
    pub workload: String,
    pub layout: String,
    pub cell: String,
    pub round: u32,
    pub position: usize,
    pub arm: String,
}

impl Step {
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.workload, self.layout, self.cell, self.round, self.position
        )
    }
}

pub(super) fn warmups(campaign: &Campaign) -> Vec<Step> {
    plan(campaign, WARMUP_PAIRS)
}

pub(super) fn measurements(campaign: &Campaign) -> Vec<Step> {
    plan(campaign, campaign.rounds)
}

fn plan(campaign: &Campaign, rounds: u32) -> Vec<Step> {
    let mut steps = Vec::new();
    for (workload, definition) in &campaign.workloads {
        for layout in definition.commands.keys() {
            for &(left, right) in &CELLS {
                let arms = [left, right];
                for round in 0..rounds {
                    for (position, index) in ORDER[round as usize % ORDER.len()].into_iter().enumerate() {
                        steps.push(Step {
                            workload: workload.clone(),
                            layout: layout.clone(),
                            cell: format!("{left}{right}"),
                            round,
                            position,
                            arm: arms[index].to_owned(),
                        });
                    }
                }
            }
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::ORDER;

    #[test]
    fn crossed_order_balances_position_and_temporal_strata() {
        assert_eq!(ORDER, [[0, 1], [1, 0], [1, 0], [0, 1]]);
        assert_eq!(ORDER.iter().filter(|pair| pair[0] == 0).count(), 2);
    }
}
