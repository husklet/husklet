use crate::suite::Error;
use clap::ValueEnum;
use serde::Deserialize;
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

    const fn build(self) -> &'static str {
        match self {
            Self::Release => "cargo build --release -p testing --bins",
            Self::Debug => "cargo build -p testing --bins",
        }
    }

    /// The engine is linked into this binary, so the requested profile is an assertion about it.
    pub(crate) fn require(self) -> Result<(), Error> {
        if self.name() == PROFILE {
            return Ok(());
        }
        Err(format!(
            "runtime sweep requested a {} engine but this runner was built as {PROFILE}; \
             build it with `{}` and run that binary, \
             or pass --engine-profile {PROFILE} to accept {PROFILE} timings",
            self.name(),
            self.build()
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
    if PROFILE != "release" {
        return Ok(hex(&Sha256::digest(&bytes)));
    }
    release_identity(&path, &bytes)
}

fn release_identity(path: &std::path::Path, bytes: &[u8]) -> Result<String, Error> {
    let bin = path.parent().ok_or("release runtime runner has no bin directory")?;
    let prefix = bin.parent().ok_or("release runtime runner has no artifact prefix")?;
    if bin.file_name().and_then(|name| name.to_str()) != Some("bin")
        || path.file_name().and_then(|name| name.to_str()) != Some("testing")
    {
        return Err("release runtime corpus must execute an immutable <prefix>/bin/testing artifact".into());
    }
    let receipt: Receipt = serde_yaml::from_str(&std::fs::read_to_string(prefix.join("receipt.yaml"))?)?;
    if receipt.schema != "husklet-runtime-corpus-artifacts-v1"
        || receipt.profile != "release"
        || receipt.runner.path != "bin/testing"
        || receipt.library.path != native_library_path()
    {
        return Err("runtime corpus artifact receipt has an invalid layout or schema".into());
    }
    let library = std::fs::read(prefix.join(&receipt.library.path))?;
    if hex(&Sha256::digest(bytes)) != receipt.runner.sha256
        || u64::try_from(bytes.len())? != receipt.runner.bytes
        || hex(&Sha256::digest(&library)) != receipt.library.sha256
        || u64::try_from(library.len())? != receipt.library.bytes
    {
        return Err("runtime corpus runner or native library differs from its immutable receipt".into());
    }
    let mut digest = Sha256::new();
    hash_field(&mut digest, b"husklet-runtime-artifact-set-v1");
    hash_field(&mut digest, bytes);
    hash_field(&mut digest, &library);
    Ok(hex(&digest.finalize()))
}

#[derive(Deserialize)]
struct Receipt {
    schema: String,
    profile: String,
    runner: Artifact,
    library: Artifact,
}

#[derive(Deserialize)]
struct Artifact {
    path: String,
    sha256: String,
    bytes: u64,
}

fn hash_field(digest: &mut impl sha2::Digest, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const fn native_library_path() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "lib/libhl_native_engine.dylib"
    }
    #[cfg(target_os = "linux")]
    {
        "lib/libhl_native_engine.so"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        "bin/hl_native_engine.dll"
    }
}

#[cfg(test)]
mod tests {
    use super::{PROFILE, Requested, identity, release_identity};
    use sha2::{Digest as _, Sha256};

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
        if PROFILE == "release" {
            let error = identity().unwrap_err().to_string();
            assert!(error.contains("immutable <prefix>/bin/testing"), "{error}");
        } else {
            assert_eq!(identity().unwrap(), identity().unwrap());
            assert_eq!(identity().unwrap().len(), 64);
        }
    }

    #[test]
    fn release_identity_binds_the_runner_and_private_library_to_the_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let runner = directory.path().join("bin/testing");
        let library = directory.path().join(super::native_library_path());
        std::fs::create_dir_all(runner.parent().unwrap()).unwrap();
        std::fs::create_dir_all(library.parent().unwrap()).unwrap();
        let runner_bytes = b"runner";
        let library_bytes = b"native-library";
        std::fs::write(&runner, runner_bytes).unwrap();
        std::fs::write(&library, library_bytes).unwrap();
        let hash = |bytes: &[u8]| {
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        std::fs::write(
            directory.path().join("receipt.yaml"),
            format!(
                "schema: husklet-runtime-corpus-artifacts-v1\nprofile: release\nrunner:\n  path: bin/testing\n  sha256: {}\n  bytes: {}\nlibrary:\n  path: {}\n  sha256: {}\n  bytes: {}\n",
                hash(runner_bytes),
                runner_bytes.len(),
                super::native_library_path(),
                hash(library_bytes),
                library_bytes.len()
            ),
        )
        .unwrap();
        assert!(release_identity(&runner, runner_bytes).is_ok());

        std::fs::write(&library, b"wrong-native-library").unwrap();
        assert!(release_identity(&runner, runner_bytes).is_err());
        std::fs::remove_file(&library).unwrap();
        assert!(release_identity(&runner, runner_bytes).is_err());
    }
}
