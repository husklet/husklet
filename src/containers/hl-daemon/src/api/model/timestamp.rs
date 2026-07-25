use std::fmt;

#[derive(Clone, Copy)]
pub(super) struct Timestamp(std::time::SystemTime);

impl Timestamp {
    pub(super) fn from_millis(milliseconds: u64) -> Self {
        Self(
            std::time::UNIX_EPOCH
                .checked_add(std::time::Duration::from_millis(milliseconds))
                .unwrap_or(std::time::UNIX_EPOCH),
        )
    }
}

impl From<std::time::SystemTime> for Timestamp {
    fn from(value: std::time::SystemTime) -> Self {
        Self(value)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanoseconds = match self.0.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => i128::try_from(duration.as_nanos()).unwrap_or(i128::MAX),
            Err(error) => -i128::try_from(error.duration().as_nanos()).unwrap_or(i128::MAX),
        };
        let seconds = i64::try_from(nanoseconds.div_euclid(1_000_000_000)).unwrap_or_else(|_| {
            if nanoseconds.is_negative() {
                i64::MIN
            } else {
                i64::MAX
            }
        });
        let fraction = nanoseconds.rem_euclid(1_000_000_000);
        let days = seconds.div_euclid(86_400);
        let day_seconds = seconds.rem_euclid(86_400);
        let shifted = days + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prime = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
        let month = month_prime + if month_prime < 10 { 3 } else { -9 };
        year += i64::from(month <= 2);
        let hour = day_seconds / 3600;
        let minute = day_seconds % 3600 / 60;
        let second = day_seconds % 60;
        write!(
            formatter,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{fraction:09}Z"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Timestamp;

    #[test]
    fn formats_epoch_and_known_dates() {
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH).to_string(),
            "1970-01-01T00:00:00.000000000Z"
        );
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000))
                .to_string(),
            "2023-11-14T22:13:20.000000000Z"
        );
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_709_164_800))
                .to_string(),
            "2024-02-29T00:00:00.000000000Z"
        );
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_709_210_096))
                .to_string(),
            "2024-02-29T12:34:56.000000000Z"
        );
    }

    #[test]
    fn formats_pre_epoch_time() {
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH - std::time::Duration::from_secs(1)).to_string(),
            "1969-12-31T23:59:59.000000000Z"
        );
    }

    #[test]
    fn preserves_nanosecond_precision() {
        let base = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let first = Timestamp::from(base + std::time::Duration::from_nanos(5)).to_string();
        let second = Timestamp::from(base + std::time::Duration::from_nanos(500)).to_string();
        assert_eq!(first, "2023-11-14T22:13:20.000000005Z");
        assert_eq!(second, "2023-11-14T22:13:20.000000500Z");
        assert_ne!(first, second);
    }

    #[test]
    fn zero_pads_fraction_to_nine_digits() {
        assert_eq!(
            Timestamp::from(
                std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_700_000_000)
                    + std::time::Duration::from_nanos(123)
            )
            .to_string(),
            "2023-11-14T22:13:20.000000123Z"
        );
    }
}
