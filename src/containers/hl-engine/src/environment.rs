//! Explicitly captured bootstrap environment.

use std::collections::BTreeMap;

pub const ACTIVATION_DESCRIPTOR_NAME: &str = "HL_ACTIVATION_FD";
pub const DEBUG_LOG_NAME: &str = "HL_LOG";
pub const AUTHORITY_DESCRIPTOR_NAME: &str = "HL_AUTHORITY_FD";
pub const AUTHORITY_HEALTH_NAME: &str = "HL_AUTHORITY_HEALTH_FD";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootstrapEnvironment {
    values: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationDescriptor {
    Absent,
    Present(i64),
    Invalid,
}

pub type AuthorityDescriptor = ActivationDescriptor;

impl BootstrapEnvironment {
    pub fn capture<I, K, V>(values: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            values: values
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    #[must_use]
    pub fn debug_log(&self) -> Option<&str> {
        self.values.get(DEBUG_LOG_NAME).map(String::as_str)
    }

    /// Consumes the activation descriptor even when its value is malformed.
    pub fn take_activation_descriptor(&mut self) -> ActivationDescriptor {
        let Some(value) = self.values.remove(ACTIVATION_DESCRIPTOR_NAME) else {
            return ActivationDescriptor::Absent;
        };
        Self::descriptor(&value)
    }

    fn descriptor(value: &str) -> ActivationDescriptor {
        let value = value.trim_start_matches(|character: char| character.is_ascii_whitespace());
        let bytes = value.as_bytes();
        let digits = match bytes.first() {
            Some(b'+') | Some(b'-') => &bytes[1..],
            _ => bytes,
        };
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return ActivationDescriptor::Invalid;
        }
        value
            .parse()
            .map_or(ActivationDescriptor::Invalid, ActivationDescriptor::Present)
    }

    pub fn take_authority_descriptor(&mut self) -> AuthorityDescriptor {
        self.take_descriptor(AUTHORITY_DESCRIPTOR_NAME)
    }

    pub fn take_authority_health(&mut self) -> AuthorityDescriptor {
        self.take_descriptor(AUTHORITY_HEALTH_NAME)
    }

    fn take_descriptor(&mut self, name: &str) -> ActivationDescriptor {
        let Some(value) = self.values.remove(name) else {
            return ActivationDescriptor::Absent;
        };
        Self::descriptor(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_debug_log() {
        let environment = BootstrapEnvironment::capture([("HL_LOG", "syscall")]);
        assert_eq!(environment.debug_log(), Some("syscall"));
    }

    #[test]
    fn activation_descriptor_is() {
        let mut valid = BootstrapEnvironment::capture([("HL_ACTIVATION_FD", "-1")]);
        assert_eq!(valid.take_activation_descriptor(), ActivationDescriptor::Present(-1));
        assert_eq!(valid.take_activation_descriptor(), ActivationDescriptor::Absent);

        let mut invalid = BootstrapEnvironment::capture([("HL_ACTIVATION_FD", "12x")]);
        assert_eq!(invalid.take_activation_descriptor(), ActivationDescriptor::Invalid);
        assert_eq!(invalid.take_activation_descriptor(), ActivationDescriptor::Absent);
    }

    #[test]
    fn activation_descriptor_requires() {
        for value in ["", "+", "-", "2 ", "1\n", "0x10"] {
            let mut environment = BootstrapEnvironment::capture([("HL_ACTIVATION_FD", value)]);
            assert_eq!(environment.take_activation_descriptor(), ActivationDescriptor::Invalid);
        }
        let mut environment = BootstrapEnvironment::capture([("HL_ACTIVATION_FD", "+42")]);
        assert_eq!(
            environment.take_activation_descriptor(),
            ActivationDescriptor::Present(42)
        );
        let mut environment = BootstrapEnvironment::capture([("HL_ACTIVATION_FD", " \t-7")]);
        assert_eq!(
            environment.take_activation_descriptor(),
            ActivationDescriptor::Present(-7)
        );
    }
}
