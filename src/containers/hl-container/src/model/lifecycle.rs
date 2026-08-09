use serde::{Deserialize, Serialize};

/// Any Linux signal number accepted by container lifecycle operations.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Signal(u8);

impl Signal {
    pub const HANGUP: Self = Self(1);
    pub const INTERRUPT: Self = Self(2);
    pub const QUIT: Self = Self(3);
    pub const KILL: Self = Self(9);
    pub const USER1: Self = Self(10);
    pub const USER2: Self = Self(12);
    pub const TERMINATE: Self = Self(15);
    /// Highest signal the Linux ABI accepts; `kill(2)` returns `EINVAL` above it.
    pub const MAXIMUM: u8 = 64;

    /// Returns the signal for a number in `1..=64`, and `None` outside that range.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= Self::MAXIMUM {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for Signal {
    fn default() -> Self {
        Self::TERMINATE
    }
}

impl Serialize for Signal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Signal {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Number(u8),
            /// Records written before signals were numbers carry a `snake_case` variant name.
            Legacy(String),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Number(number) => Self::new(number).ok_or_else(|| serde::de::Error::custom("signal out of range")),
            Wire::Legacy(name) => match name.as_str() {
                "terminate" => Ok(Self::TERMINATE),
                "kill" => Ok(Self::KILL),
                "interrupt" => Ok(Self::INTERRUPT),
                "quit" => Ok(Self::QUIT),
                "hangup" => Ok(Self::HANGUP),
                "user1" => Ok(Self::USER1),
                "user2" => Ok(Self::USER2),
                _ => Err(serde::de::Error::custom("unknown signal name")),
            },
        }
    }
}

/// State transition observed by a waiter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WaitCondition {
    /// Return when the process is no longer running (or was already exited).
    #[default]
    NotRunning,
    /// Return after the next process generation exits, even if it will restart.
    NextExit,
    /// Return only after the container metadata has been removed.
    Removed,
}

/// Durable ownership policy applied after terminal process completion.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalPolicy {
    #[default]
    Retain,
    Automatic,
}

#[cfg(test)]
mod tests {
    use super::Signal;

    /// The host kernel accepts `1..=64` and answers `EINVAL` outside it (measured with
    /// `kill(2)` against a live child on this box), so the value type carries the same bound.
    #[test]
    fn signal_range_matches_the_linux_abi() {
        assert_eq!(Signal::new(1).unwrap().get(), 1);
        assert_eq!(Signal::new(64).unwrap().get(), 64);
        assert_eq!(Signal::new(34).unwrap().get(), 34);
        assert!(Signal::new(0).is_none());
        assert!(Signal::new(65).is_none());
        assert_eq!(Signal::default(), Signal::TERMINATE);
        assert_eq!(Signal::TERMINATE.get(), 15);
        assert_eq!(Signal::KILL.get(), 9);
        assert_eq!(Signal::USER2.get(), 12);
    }

    /// Durable specs written before signals became numbers store a `snake_case` variant name.
    #[test]
    fn durable_records_read_legacy_names_and_write_numbers() {
        for (legacy, expected) in [
            ("\"terminate\"", Signal::TERMINATE),
            ("\"kill\"", Signal::KILL),
            ("\"interrupt\"", Signal::INTERRUPT),
            ("\"quit\"", Signal::QUIT),
            ("\"hangup\"", Signal::HANGUP),
            ("\"user1\"", Signal::USER1),
            ("\"user2\"", Signal::USER2),
        ] {
            assert_eq!(serde_json::from_str::<Signal>(legacy).unwrap(), expected, "{legacy}");
        }
        assert!(serde_json::from_str::<Signal>("\"bogus\"").is_err());
        assert!(serde_json::from_str::<Signal>("0").is_err());
        assert!(serde_json::from_str::<Signal>("65").is_err());
        for number in 1..=Signal::MAXIMUM {
            let signal = Signal::new(number).unwrap();
            let encoded = serde_json::to_string(&signal).unwrap();
            assert_eq!(encoded, number.to_string());
            assert_eq!(serde_json::from_str::<Signal>(&encoded).unwrap(), signal);
        }
    }
}
