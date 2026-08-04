/// A concrete OCI target platform.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unsupported platform {value:?}; expected linux/ARCH[/VARIANT]")]
pub struct PlatformError {
    value: String,
}

impl Platform {
    #[must_use]
    pub fn linux_arm64() -> Self {
        Self::new("linux", "arm64", None)
    }
    #[must_use]
    pub fn linux_amd64() -> Self {
        Self::new("linux", "amd64", None)
    }
    pub fn new(os: impl Into<String>, architecture: impl Into<String>, variant: Option<String>) -> Self {
        Self {
            os: os.into(),
            architecture: architecture.into(),
            variant,
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = PlatformError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('/');
        let os = parts.next().unwrap_or_default();
        let architecture = parts.next().unwrap_or_default();
        let variant = parts.next();
        if os != "linux" || architecture.is_empty() || variant.is_some_and(str::is_empty) || parts.next().is_some() {
            return Err(PlatformError {
                value: value.to_owned(),
            });
        }
        let architecture = match architecture.to_ascii_lowercase().as_str() {
            "aarch64" => "arm64",
            "x86_64" | "x86-64" => "amd64",
            _ => architecture,
        };
        Ok(Self::new(os, architecture, variant.map(str::to_owned)))
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.os, self.architecture)?;
        if let Some(variant) = &self.variant {
            write!(formatter, "/{variant}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn parses_supported_linux_platforms() {
        assert_eq!("linux/arm64".parse::<Platform>().unwrap(), Platform::linux_arm64());
        assert_eq!(
            "linux/arm/v7".parse::<Platform>().unwrap(),
            Platform::new("linux", "arm", Some("v7".into()))
        );
        for invalid in ["", "darwin/arm64", "linux", "linux/", "linux/arm64/"] {
            assert!(invalid.parse::<Platform>().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn normalizes_linux_architecture_aliases() {
        for value in ["linux/arm64", "linux/aarch64"] {
            assert_eq!(value.parse::<Platform>().unwrap(), Platform::linux_arm64());
        }
        for value in ["linux/amd64", "linux/x86_64", "linux/x86-64"] {
            assert_eq!(value.parse::<Platform>().unwrap(), Platform::linux_amd64());
        }
        assert_eq!(
            "linux/arm64/v8".parse::<Platform>().unwrap(),
            Platform::new("linux", "arm64", Some("v8".into()))
        );
    }

    #[test]
    fn rejects_non_linux_and_incomplete_platforms() {
        for invalid in ["darwin/arm64", "windows/amd64", "linux", "linux/"] {
            assert!(invalid.parse::<Platform>().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn oci_platform_names_round_trip() {
        for platform in [
            Platform::linux_arm64(),
            Platform::linux_amd64(),
            Platform::new("linux", "arm", Some("v7".into())),
        ] {
            assert_eq!(platform.to_string().parse::<Platform>().unwrap(), platform);
        }
    }
}
