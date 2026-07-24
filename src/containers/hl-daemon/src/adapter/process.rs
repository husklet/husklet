#[derive(Clone, Copy, Default)]
pub(crate) struct Sample {
    pub(crate) memory: u64,
    pub(crate) cpu_seconds: u64,
}

impl Sample {
    pub(crate) fn read(process_id: u64) -> Self {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=,time=", "-p", &process_id.to_string()])
            .output();
        let Ok(output) = output else {
            return Self::default();
        };
        if !output.status.success() {
            return Self::default();
        }
        let output = String::from_utf8_lossy(&output.stdout);
        let mut fields = output.split_whitespace();
        Self {
            memory: fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default()
                .saturating_mul(1024),
            cpu_seconds: fields.next().map_or(0, |value| CpuSeconds::parse(value).0),
        }
    }
}

struct CpuSeconds(u64);

impl CpuSeconds {
    fn parse(value: &str) -> Self {
        let (days, value) = value.split_once('-').map_or((0, value), |(days, value)| {
            (days.parse::<u64>().unwrap_or_default(), value)
        });
        Self(
            value
                .split(':')
                .fold(0_u64, |total, value| {
                    total
                        .saturating_mul(60)
                        .saturating_add(value.split('.').next().unwrap_or("0").parse().unwrap_or(0))
                })
                .saturating_add(days.saturating_mul(86_400)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::CpuSeconds;

    #[test]
    fn parses_process_cpu_clock_formats() {
        assert_eq!(CpuSeconds::parse("01:02").0, 62);
        assert_eq!(CpuSeconds::parse("1:02:03").0, 3_723);
        assert_eq!(CpuSeconds::parse("2-01:02:03.99").0, 176_523);
    }
}
