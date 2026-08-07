use crate::suite::Error;
use clap::ValueEnum;
use sha2::{Digest as _, Sha256};

/// The cargo profile the engine this runner links was compiled with.
pub(crate) const PROFILE: &str = env!("HL_TESTING_PROFILE");

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Requested {
    Release,
    Debug,
}

impl Requested {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Debug => "debug",
        }
    }

    /// The engine is linked into this binary, so the requested profile is an assertion about it.
    pub(crate) fn require(self) -> Result<(), Error> {
        if self.name() == PROFILE {
            return Ok(());
        }
        Err(format!(
            "runtime sweep requested a {} engine but this runner was built as {PROFILE}; \
             build it with `cargo build --profile {} -p testing --bins` and run that binary, \
             or pass --engine-profile {PROFILE} to accept {PROFILE} timings",
            self.name(),
            self.name()
        )
        .into())
    }
}

/// The single ambient read of this process's own image, shared by worker spawn and run identity.
pub(crate) fn runner() -> Result<std::path::PathBuf, Error> {
    std::env::current_exe().map_err(|error| format!("resolve runtime runner: {error}").into())
}

/// Identifies the exact runner binary, so a stale build cannot silently resume another one's rows.
pub(crate) fn identity() -> Result<String, Error> {
    let path = runner()?;
    let bytes = std::fs::read(&path).map_err(|error| format!("read runner {}: {error}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::{PROFILE, Requested, identity};

    #[test]
    fn the_built_profile_is_named_and_accepted_only_by_itself() {
        assert!(PROFILE == "debug" || PROFILE == "release", "{PROFILE}");
        let (same, other) = if PROFILE == "release" {
            (Requested::Release, Requested::Debug)
        } else {
            (Requested::Debug, Requested::Release)
        };
        same.require().unwrap();
        let error = other.require().unwrap_err().to_string();
        assert!(error.contains(PROFILE), "{error}");
    }

    #[test]
    fn the_runner_identity_is_a_stable_digest() {
        assert_eq!(identity().unwrap(), identity().unwrap());
        assert_eq!(identity().unwrap().len(), 64);
    }
}
