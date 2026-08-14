//! Paths an extension may name. Invalid ones cannot be constructed.

/// A path relative to a declared root, guaranteed not to escape it.
///
/// Traversal, absolute paths, roots, and interior NUL bytes are rejected at
/// construction, so no later code has to remember to check. Resolution against
/// a real directory still canonicalizes and re-checks the prefix, because a
/// symbolic link can escape a syntactically valid path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize)]
#[serde(transparent)]
pub struct RelativePath(String);

impl RelativePath {
    /// Longest path accepted, so a manifest cannot force an unbounded buffer.
    pub const LIMIT: usize = 4096;

    /// # Errors
    /// Returns why the path was refused.
    pub fn new(value: impl Into<String>) -> Result<Self, Refusal> {
        let value = value.into();
        if value.is_empty() {
            return Err(Refusal::Empty);
        }
        if value.len() > Self::LIMIT {
            return Err(Refusal::TooLong(value.len()));
        }
        if value.contains('\0') {
            return Err(Refusal::Interior);
        }
        if value.starts_with('/') || value.starts_with('\\') {
            return Err(Refusal::Absolute);
        }
        if Self::is_windows_root(&value) {
            return Err(Refusal::Absolute);
        }
        for part in value.split(['/', '\\']) {
            if part == ".." {
                return Err(Refusal::Traversal);
            }
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The path split into its components, empty and `.` segments dropped.
    #[must_use]
    pub fn parts(&self) -> Vec<&str> {
        self.0
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
            .collect()
    }

    /// Whether this path lies at or below `root`.
    #[must_use]
    pub fn within(&self, root: &Self) -> bool {
        let root = root.parts();
        let parts = self.parts();
        parts.len() >= root.len() && parts[..root.len()] == root[..]
    }

    fn is_windows_root(value: &str) -> bool {
        let mut characters = value.chars();
        matches!(
            (characters.next(), characters.next(), characters.next()),
            (Some(letter), Some(':'), Some('/' | '\\')) if letter.is_ascii_alphabetic()
        )
    }
}

impl std::fmt::Display for RelativePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for RelativePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Why a path was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Empty,
    Absolute,
    Traversal,
    Interior,
    TooLong(usize),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "path is empty"),
            Self::Absolute => write!(formatter, "path is absolute"),
            Self::Traversal => write!(formatter, "path contains a parent traversal"),
            Self::Interior => write!(formatter, "path contains a NUL byte"),
            Self::TooLong(length) => {
                write!(
                    formatter,
                    "path is {length} bytes, above the {} limit",
                    RelativePath::LIMIT
                )
            }
        }
    }
}

impl std::error::Error for Refusal {}

#[cfg(test)]
mod tests {
    use super::{Refusal, RelativePath};

    #[test]
    fn ordinary_relative_paths_are_accepted() {
        assert!(RelativePath::new("logs/app.log").is_ok());
        assert!(RelativePath::new("./config").is_ok());
        assert!(RelativePath::new("a/b/c.txt").is_ok());
    }

    #[test]
    fn escapes_are_refused_at_construction() {
        assert_eq!(RelativePath::new("../secret"), Err(Refusal::Traversal));
        assert_eq!(RelativePath::new("logs/../../secret"), Err(Refusal::Traversal));
        assert_eq!(RelativePath::new("logs\\..\\secret"), Err(Refusal::Traversal));
        assert_eq!(RelativePath::new("/etc/passwd"), Err(Refusal::Absolute));
        assert_eq!(RelativePath::new("\\\\host\\share"), Err(Refusal::Absolute));
        assert_eq!(RelativePath::new("C:\\Windows"), Err(Refusal::Absolute));
        assert_eq!(RelativePath::new("a\0b"), Err(Refusal::Interior));
        assert_eq!(RelativePath::new(""), Err(Refusal::Empty));
    }

    #[test]
    fn a_name_merely_starting_with_dots_is_not_a_traversal() {
        assert!(RelativePath::new("..hidden").is_ok());
        assert!(RelativePath::new("logs/...state").is_ok());
    }

    #[test]
    fn containment_is_decided_by_whole_components() {
        let root = RelativePath::new("logs").expect("root");
        assert!(RelativePath::new("logs/app.log").expect("path").within(&root));
        assert!(RelativePath::new("logs").expect("path").within(&root));
        assert!(
            !RelativePath::new("logsother/app.log").expect("path").within(&root),
            "a shared prefix is not containment"
        );
        assert!(!RelativePath::new("etc/passwd").expect("path").within(&root));
    }

    #[test]
    fn an_over_long_path_is_refused_before_it_is_stored() {
        let long = "a".repeat(RelativePath::LIMIT + 1);
        assert_eq!(RelativePath::new(long), Err(Refusal::TooLong(RelativePath::LIMIT + 1)));
    }

    #[test]
    fn deserializing_applies_the_same_refusals() {
        let refused: Result<RelativePath, _> = serde_json::from_str("\"../secret\"");
        assert!(refused.is_err(), "a wire path must not bypass construction");
        let accepted: RelativePath = serde_json::from_str("\"logs/app.log\"").expect("valid");
        assert_eq!(accepted.as_str(), "logs/app.log");
    }
}
