use super::PROTOCOL;
use crate::config::WorkspaceConfig;
use sha2::Digest as _;
use std::io;

const ABI: &str = "workspace-runtime-1";

pub(super) struct RuntimeIdentity(String);

impl RuntimeIdentity {
    pub(super) fn current(workspace: &WorkspaceConfig) -> io::Result<Self> {
        let drivers = crate::runtime::drivers::Drivers::fingerprint(
            crate::paths::drivers_dir(),
            workspace.arch,
            workspace.gui,
            workspace.cuda.is_some(),
        )?;
        let mut digest = sha2::Sha256::new();
        Self::field(&mut digest, ABI.as_bytes());
        Self::field(&mut digest, PROTOCOL.as_bytes());
        Self::field(&mut digest, env!("CARGO_PKG_VERSION").as_bytes());
        Self::field(&mut digest, env!("HUSKLET_RUNTIME_BUILD_ID").as_bytes());
        Self::field(&mut digest, drivers.as_bytes());
        let identity = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self(identity))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    fn field(digest: &mut sha2::Sha256, value: &[u8]) {
        digest.update(value.len().to_le_bytes());
        digest.update(value);
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeIdentity;

    #[test]
    fn runtime_identity_is_stable_for_one_executable_generation() {
        let workspace = crate::config::WorkspaceConfig::new("demo", "ubuntu", hl_ws::Arch::Arm64);
        let first = RuntimeIdentity::current(&workspace).unwrap();
        let second = RuntimeIdentity::current(&workspace).unwrap();
        assert_eq!(first.as_str(), second.as_str());
        assert_eq!(first.as_str().len(), 64);
    }
}
