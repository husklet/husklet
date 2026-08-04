use crate::suite::Error;
use serde::Deserialize;
use std::time::Duration;

const MAX_DURATION_SECONDS: u64 = 3_600;
const MAX_REPETITIONS: u16 = 100;
const MAX_CPU: u16 = 256;
const MAX_MEMORY_MIB: u32 = 1_048_576;
const MAX_PROCESSES: u32 = 65_536;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Plan {
    duration_seconds: u64,
    repetitions: u16,
    resources: Resources,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Resources {
    cpu: u16,
    memory_mib: u32,
    processes: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Attempt {
    ordinal: u16,
}

impl Plan {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if !(1..=MAX_DURATION_SECONDS).contains(&self.duration_seconds) {
            return Err(format!("soak duration_seconds must be between 1 and {MAX_DURATION_SECONDS}").into());
        }
        if !(1..=MAX_REPETITIONS).contains(&self.repetitions) {
            return Err(format!("soak repetitions must be between 1 and {MAX_REPETITIONS}").into());
        }
        self.resources.validate()?;
        self.duration_seconds
            .checked_mul(u64::from(self.repetitions))
            .ok_or("soak total duration overflows")?;
        Ok(())
    }

    pub(crate) const fn duration(&self) -> Duration {
        Duration::from_secs(self.duration_seconds)
    }

    pub(crate) fn total_duration(&self) -> Duration {
        Duration::from_secs(self.duration_seconds * u64::from(self.repetitions))
    }

    pub(crate) const fn resources(&self) -> Resources {
        self.resources
    }

    pub(crate) fn attempts(&self) -> impl ExactSizeIterator<Item = Attempt> {
        (1..=self.repetitions).map(|ordinal| Attempt { ordinal })
    }

    pub(crate) const fn repetitions(&self) -> u16 {
        self.repetitions
    }
}

impl Resources {
    fn validate(self) -> Result<(), Error> {
        if !(1..=MAX_CPU).contains(&self.cpu) {
            return Err(format!("soak resources.cpu must be between 1 and {MAX_CPU}").into());
        }
        if !(1..=MAX_MEMORY_MIB).contains(&self.memory_mib) {
            return Err(format!("soak resources.memory_mib must be between 1 and {MAX_MEMORY_MIB}").into());
        }
        if !(1..=MAX_PROCESSES).contains(&self.processes) {
            return Err(format!("soak resources.processes must be between 1 and {MAX_PROCESSES}").into());
        }
        Ok(())
    }

    pub(crate) const fn cpu(self) -> u16 {
        self.cpu
    }

    pub(crate) const fn memory_mib(self) -> u32 {
        self.memory_mib
    }

    pub(crate) const fn processes(self) -> u32 {
        self.processes
    }
}

impl Attempt {
    pub(crate) const fn ordinal(self) -> u16 {
        self.ordinal
    }
}

#[cfg(test)]
mod tests {
    use super::Plan;

    fn parse(yaml: &str) -> Plan {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn bounded_and_extended_profiles_have_deterministic_attempts() {
        let bounded =
            parse("duration_seconds: 240\nrepetitions: 1\nresources:\n  cpu: 1\n  memory_mib: 1024\n  processes: 64\n");
        bounded.validate().unwrap();
        assert_eq!(bounded.duration().as_secs(), 240);
        assert_eq!(
            bounded.attempts().map(|attempt| attempt.ordinal()).collect::<Vec<_>>(),
            vec![1]
        );

        let extended = parse(
            "duration_seconds: 240\nrepetitions: 10\nresources:\n  cpu: 4\n  memory_mib: 4096\n  processes: 256\n",
        );
        extended.validate().unwrap();
        assert_eq!(extended.total_duration().as_secs(), 2_400);
        assert_eq!(
            extended.attempts().map(|attempt| attempt.ordinal()).collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>()
        );
        assert_eq!(extended.resources().cpu(), 4);
        assert_eq!(extended.resources().memory_mib(), 4096);
        assert_eq!(extended.resources().processes(), 256);
    }

    #[test]
    fn zero_and_excessive_values_fail_closed() {
        for yaml in [
            "duration_seconds: 0\nrepetitions: 1\nresources:\n  cpu: 1\n  memory_mib: 1\n  processes: 1\n",
            "duration_seconds: 3601\nrepetitions: 1\nresources:\n  cpu: 1\n  memory_mib: 1\n  processes: 1\n",
            "duration_seconds: 1\nrepetitions: 101\nresources:\n  cpu: 1\n  memory_mib: 1\n  processes: 1\n",
            "duration_seconds: 1\nrepetitions: 1\nresources:\n  cpu: 0\n  memory_mib: 1\n  processes: 1\n",
            "duration_seconds: 1\nrepetitions: 1\nresources:\n  cpu: 1\n  memory_mib: 1048577\n  processes: 1\n",
            "duration_seconds: 1\nrepetitions: 1\nresources:\n  cpu: 1\n  memory_mib: 1\n  processes: 65537\n",
        ] {
            assert!(parse(yaml).validate().is_err(), "accepted {yaml:?}");
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let result = serde_yaml::from_str::<Plan>(
            "duration_seconds: 1\nrepetitions: 1\nresources:\n  cpu: 1\n  memory_mib: 1\n  processes: 1\nextra: true\n",
        );
        assert!(result.is_err());
    }
}
