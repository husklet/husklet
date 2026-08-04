#[cfg(feature = "runtime")]
use super::timestamp::Timestamp;
use serde::{Deserialize, Serialize};

/// Docker's container-path metadata header payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStat {
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub mtime: String,
    pub link_target: String,
}

#[cfg(feature = "runtime")]
impl PathStat {
    pub(crate) fn header(&self) -> Result<String, serde_json::Error> {
        use base64::Engine as _;
        serde_json::to_vec(self).map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

#[cfg(feature = "runtime")]
impl From<hl_container::Stat> for PathStat {
    fn from(value: hl_container::Stat) -> Self {
        Self {
            name: value.name,
            size: value.size,
            mode: FileMode::from(value.mode).into(),
            mtime: Timestamp::from(value.modified).to_string(),
            link_target: value
                .link
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
        }
    }
}

#[derive(Clone, Copy)]
struct FileMode(u32);

impl From<u32> for FileMode {
    fn from(mode: u32) -> Self {
        let kind = match mode & 0o170_000 {
            0o040_000 => 1 << 31,
            0o120_000 => 1 << 27,
            0o060_000 => 1 << 26,
            0o020_000 => 1 << 25,
            0o010_000 => 1 << 24,
            0o140_000 => 1 << 23,
            _ => 0,
        };
        Self(kind | (mode & 0o7777))
    }
}

impl From<FileMode> for u32 {
    fn from(value: FileMode) -> Self {
        value.0
    }
}
/// Docker's filesystem change classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(into = "i64", try_from = "i64")]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
}

impl From<ChangeKind> for i64 {
    fn from(value: ChangeKind) -> Self {
        match value {
            ChangeKind::Modified => 0,
            ChangeKind::Added => 1,
            ChangeKind::Deleted => 2,
        }
    }
}

impl TryFrom<i64> for ChangeKind {
    type Error = String;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Modified),
            1 => Ok(Self::Added),
            2 => Ok(Self::Deleted),
            _ => Err(format!("invalid filesystem change kind {value}")),
        }
    }
}

/// One Docker-compatible changed path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[cfg(feature = "runtime")]
impl From<hl_container::Change> for Change {
    fn from(value: hl_container::Change) -> Self {
        Self {
            path: value.path.to_string_lossy().into_owned(),
            kind: match value.kind {
                hl_container::ChangeKind::Modified => ChangeKind::Modified,
                hl_container::ChangeKind::Added => ChangeKind::Added,
                hl_container::ChangeKind::Deleted => ChangeKind::Deleted,
            },
        }
    }
}

#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::FileMode;
    use crate::api::model::timestamp::Timestamp;

    #[test]
    fn docker_path_metadata_uses_go_modes_and_rfc3339() {
        assert_eq!(u32::from(FileMode::from(0o100_640)), 0o640);
        assert_eq!(u32::from(FileMode::from(0o040_755)), (1 << 31) | 0o755);
        assert_eq!(u32::from(FileMode::from(0o120_777)), (1 << 27) | 0o777);
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH).to_string(),
            "1970-01-01T00:00:00.000000000Z"
        );
        assert_eq!(
            Timestamp::from(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_704_067_200)).to_string(),
            "2024-01-01T00:00:00.000000000Z"
        );
    }
}
