//! Strict parser for the portable `perf stat -x TAB` event set shared by campaigns.

use crate::suite::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Counters {
    pub(crate) duration_ns: u64,
    pub(crate) task_clock_ms: f64,
    pub(crate) instructions: u64,
    pub(crate) cycles: u64,
    pub(crate) page_faults: u64,
}

pub(crate) fn parse(text: &str) -> Result<Counters, Error> {
    let value = |event: &str| -> Result<&str, Error> {
        text.lines()
            .filter(|line| !line.starts_with('#'))
            .find_map(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                fields.iter().any(|field| field.trim() == event).then(|| fields[0].trim())
            })
            .ok_or_else(|| format!("perf output omitted {event}").into())
    };
    let counters = Counters {
        duration_ns: value("duration_time")?.parse()?,
        task_clock_ms: value("task-clock")?.parse()?,
        instructions: value("instructions")?.parse()?,
        cycles: value("cycles")?.parse()?,
        page_faults: value("page-faults")?.parse()?,
    };
    if counters.duration_ns == 0
        || !counters.task_clock_ms.is_finite()
        || counters.task_clock_ms <= 0.0
        || counters.instructions == 0
        || counters.cycles == 0
        || counters.page_faults == 0
    {
        return Err("perf output contains an absent or zero counter".into());
    }
    Ok(counters)
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_portable_event_set_is_required() {
        let text = "11\t\tduration_time\n2.5\tmsec\ttask-clock\n31\t\tinstructions\n41\t\tcycles\n5\t\tpage-faults\n";
        let parsed = super::parse(text).unwrap();
        assert_eq!(parsed.instructions, 31);
        assert_eq!(parsed.page_faults, 5);
        assert!(super::parse(&text.replace("31\t\tinstructions", "0\t\tinstructions")).is_err());
        assert!(super::parse(&text.replace("41\t\tcycles\n", "")).is_err());
    }
}
