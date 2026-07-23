use std::{collections::BTreeMap, fmt, str::FromStr};

/// One validated Linux process environment assignment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvVar {
    name: String,
    value: String,
}

impl EnvVar {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl FromStr for EnvVar {
    type Err = EnvError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, value) = value
            .split_once('=')
            .ok_or_else(|| EnvError::Assignment(value.to_owned()))?;
        if name.is_empty() {
            return Err(EnvError::Name);
        }
        Ok(Self {
            name: name.to_owned(),
            value: value.to_owned(),
        })
    }
}

/// A validated process environment, indexed by variable name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvVars(BTreeMap<String, String>);

impl EnvVars {
    /// Parses Docker-style `NAME=VALUE` assignments.
    ///
    /// # Errors
    /// Returns [`EnvError`] when an assignment has no separator or has an empty name.
    pub fn parse(values: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Self, EnvError> {
        let variables = values
            .into_iter()
            .map(|value| value.as_ref().parse::<EnvVar>())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(variables.into_iter().collect())
    }

    #[must_use]
    pub fn into_inner(self) -> BTreeMap<String, String> {
        self.0
    }
}

impl FromIterator<EnvVar> for EnvVars {
    fn from_iter<T: IntoIterator<Item = EnvVar>>(values: T) -> Self {
        Self(
            values
                .into_iter()
                .map(|variable| (variable.name, variable.value))
                .collect(),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvError {
    Assignment(String),
    Name,
}

impl fmt::Display for EnvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assignment(value) => {
                write!(
                    formatter,
                    "invalid environment entry {value:?}; expected NAME=VALUE"
                )
            }
            Self::Name => formatter.write_str("environment name must not be empty"),
        }
    }
}

impl std::error::Error for EnvError {}

#[cfg(test)]
mod tests {
    use super::{EnvError, EnvVar, EnvVars};

    #[test]
    fn parses_names_and_values() {
        let variable: EnvVar = "PATH=/bin:/usr/bin".parse().unwrap();
        assert_eq!(variable.name(), "PATH");
        assert_eq!(variable.value(), "/bin:/usr/bin");
        assert_eq!(
            EnvVars::parse(["A=one", "B=two=three"])
                .unwrap()
                .into_inner(),
            [("A".into(), "one".into()), ("B".into(), "two=three".into())]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn duplicate_names_use_the_last_assignment() {
        assert_eq!(
            EnvVars::parse(["VALUE=old", "VALUE=new"])
                .unwrap()
                .into_inner()
                .get("VALUE")
                .map(String::as_str),
            Some("new")
        );
    }

    #[test]
    fn rejects_missing_assignment_separator() {
        assert_eq!(
            EnvVars::parse(["BROKEN"]).unwrap_err(),
            EnvError::Assignment("BROKEN".into())
        );
    }

    #[test]
    fn rejects_empty_names() {
        assert_eq!(EnvVars::parse(["=value"]).unwrap_err(), EnvError::Name);
    }
}
