use std::{collections::BTreeMap, env, ffi::OsString, path::PathBuf};

macro_rules! open_value {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

open_value!(TargetOs);
open_value!(TargetArch);
open_value!(TargetEnvironment);
open_value!(Triple);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    Development,
    Release,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildEnvironment {
    pub target: Triple,
    pub target_os: TargetOs,
    pub target_arch: TargetArch,
    pub target_environment: TargetEnvironment,
    pub host: Triple,
    pub output: PathBuf,
    pub manifest_directory: PathBuf,
    pub profile: Profile,
    variables: BTreeMap<OsString, OsString>,
}

impl BuildEnvironment {
    pub fn from_cargo() -> Result<Self, String> {
        Self::from_variables(env::vars_os().collect())
    }

    pub fn from_variables(variables: BTreeMap<OsString, OsString>) -> Result<Self, String> {
        Ok(Self {
            target: Triple::new(required(&variables, "TARGET")?),
            target_os: TargetOs::new(required(&variables, "CARGO_CFG_TARGET_OS")?),
            target_arch: TargetArch::new(required(&variables, "CARGO_CFG_TARGET_ARCH")?),
            target_environment: TargetEnvironment::new(required(&variables, "CARGO_CFG_TARGET_ENV")?),
            host: Triple::new(required(&variables, "HOST")?),
            output: PathBuf::from(required_os(&variables, "OUT_DIR")?),
            manifest_directory: PathBuf::from(required_os(&variables, "CARGO_MANIFEST_DIR")?),
            profile: if required(&variables, "PROFILE")? == "release" {
                Profile::Release
            } else {
                Profile::Development
            },
            variables,
        })
    }

    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.variables.contains_key(OsString::from(name).as_os_str())
    }

    #[must_use]
    pub fn feature(&self, name: &str) -> bool {
        let name = format!("CARGO_FEATURE_{}", name.replace('-', "_").to_ascii_uppercase());
        self.flag(&name)
    }

    pub fn value(&self, name: &str) -> Result<Option<&str>, String> {
        self.variables
            .get(OsString::from(name).as_os_str())
            .map(|value| value.to_str().ok_or_else(|| format!("{name} is not valid UTF-8")))
            .transpose()
    }
}

fn required(variables: &BTreeMap<OsString, OsString>, name: &str) -> Result<String, String> {
    required_os(variables, name)?
        .into_string()
        .map_err(|_| format!("{name} is not valid UTF-8"))
}

fn required_os(variables: &BTreeMap<OsString, OsString>, name: &str) -> Result<OsString, String> {
    variables
        .get(OsString::from(name).as_os_str())
        .cloned()
        .ok_or_else(|| format!("Cargo supplies {name}"))
}

#[cfg(test)]
mod tests {
    use super::{BuildEnvironment, Profile};
    use std::{collections::BTreeMap, ffi::OsString};

    fn variables() -> BTreeMap<OsString, OsString> {
        [
            ("TARGET", "riscv64-acme-newos-eabi"),
            ("CARGO_CFG_TARGET_OS", "newos"),
            ("CARGO_CFG_TARGET_ARCH", "riscv64"),
            ("CARGO_CFG_TARGET_ENV", "eabi"),
            ("HOST", "custom-builder"),
            ("OUT_DIR", "/tmp/out"),
            ("CARGO_MANIFEST_DIR", "/source/package"),
            ("PROFILE", "release"),
            ("PROJECT_SWITCH", "1"),
            ("CARGO_FEATURE_NATIVE_HOOKS", "1"),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
    }

    #[test]
    fn target_values_are_open_and_preserve_unknown_values() {
        let parsed = BuildEnvironment::from_variables(variables()).unwrap();
        assert_eq!(parsed.target_os.as_str(), "newos");
        assert_eq!(parsed.target_arch.as_str(), "riscv64");
        assert_eq!(parsed.target_environment.as_str(), "eabi");
        assert_eq!(parsed.profile, Profile::Release);
    }

    #[test]
    fn flags_and_features_are_typed_presence_queries() {
        let parsed = BuildEnvironment::from_variables(variables()).unwrap();
        assert!(parsed.flag("PROJECT_SWITCH"));
        assert!(parsed.feature("native-hooks"));
        assert!(!parsed.feature("missing"));
    }
}
