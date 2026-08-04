use std::fmt;

/// Maximum byte length accepted by the Linux open-plan boundary.
pub const PATH_MAXIMUM: usize = 4_096;
const COMPONENT_MAXIMUM: usize = 512;
const NAME_MAXIMUM: usize = 255;

/// Failure to construct a bounded guest path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathError {
    Empty,
    TooLong,
    TooManyComponents,
    ContainsNul,
    InvalidComponent,
}

/// Owned Linux pathname bytes bounded to leave room for the ABI terminator.
///
/// Linux pathnames are byte strings. They need not be UTF-8, but an owned path
/// cannot contain the NUL byte that terminates the syscall representation.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GuestPathBytes(Vec<u8>);

/// One Linux pathname component with the filesystem's byte-level bounds.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GuestName(Vec<u8>);

impl GuestName {
    pub fn new(input: &[u8]) -> Result<Self, PathError> {
        if input.is_empty() {
            return Err(PathError::Empty);
        }
        if input.len() > NAME_MAXIMUM {
            return Err(PathError::TooLong);
        }
        if input.contains(&0) {
            return Err(PathError::ContainsNul);
        }
        if input.contains(&b'/') || input == b"." || input == b".." {
            return Err(PathError::InvalidComponent);
        }
        Ok(Self(input.to_vec()))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl GuestPathBytes {
    /// Copies pathname bytes after validating Linux's `PATH_MAX` boundary.
    pub fn new(input: &[u8]) -> Result<Self, PathError> {
        if input.len() >= PATH_MAXIMUM {
            return Err(PathError::TooLong);
        }
        if input.contains(&0) {
            return Err(PathError::ContainsNul);
        }
        Ok(Self(input.to_vec()))
    }

    /// Returns the exact guest pathname bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns whether this pathname starts at the guest root.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.0.starts_with(b"/")
    }

    /// Lexically confines an absolute pathname to the guest root.
    pub(crate) fn normalize_absolute(&self) -> Result<Self, PathError> {
        if !self.is_absolute() {
            return Err(PathError::InvalidComponent);
        }
        let mut components: Vec<&[u8]> = Vec::new();
        for component in self.0.split(|byte| *byte == b'/') {
            match component {
                b"" | b"." => {}
                b".." => {
                    components.pop();
                }
                value if components.len() == COMPONENT_MAXIMUM => {
                    return Err(PathError::TooManyComponents);
                }
                value => {
                    GuestName::new(value)?;
                    components.push(value);
                }
            }
        }
        let mut normalized = Vec::with_capacity(self.0.len());
        normalized.push(b'/');
        for (index, component) in components.iter().enumerate() {
            if index != 0 {
                normalized.push(b'/');
            }
            normalized.extend_from_slice(component);
        }
        Self::new(&normalized)
    }

    /// Returns the normalized absolute parent and final component.
    pub(crate) fn parent_and_name(&self) -> Result<(Self, GuestName), PathError> {
        let normalized = self.normalize_absolute()?;
        let slash = normalized
            .0
            .iter()
            .rposition(|byte| *byte == b'/')
            .ok_or(PathError::InvalidComponent)?;
        let name = GuestName::new(&normalized.0[slash + 1..])?;
        let parent = if slash == 0 {
            Self::new(b"/")?
        } else {
            Self::new(&normalized.0[..slash])?
        };
        Ok((parent, name))
    }
}

/// A bounded guest path whose absolute form is lexically confined to `/`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GuestPath(String);

impl GuestPath {
    /// Copies and validates one guest-provided path.
    pub fn new(input: &str) -> Result<Self, PathError> {
        if input.is_empty() {
            return Err(PathError::Empty);
        }
        if input.len() > PATH_MAXIMUM {
            return Err(PathError::TooLong);
        }
        if !input.starts_with('/') {
            return Ok(Self(input.to_owned()));
        }
        Self::normalize_absolute(input)
    }

    fn normalize_absolute(input: &str) -> Result<Self, PathError> {
        let mut components = Vec::new();
        for component in input.split('/') {
            Self::apply_component(&mut components, component)?;
        }
        let normalized = if components.is_empty() {
            "/".to_owned()
        } else {
            let length =
                1 + components.iter().map(|value| value.len()).sum::<usize>() + components.len().saturating_sub(1);
            if length > PATH_MAXIMUM {
                return Err(PathError::TooLong);
            }
            let mut path = String::with_capacity(length);
            for component in components {
                path.push('/');
                path.push_str(component);
            }
            path
        };
        Ok(Self(normalized))
    }

    fn apply_component<'path>(components: &mut Vec<&'path str>, component: &'path str) -> Result<(), PathError> {
        match component {
            "" | "." => Ok(()),
            ".." => {
                components.pop();
                Ok(())
            }
            _ if components.len() == COMPONENT_MAXIMUM => Err(PathError::TooManyComponents),
            value => {
                components.push(value);
                Ok(())
            }
        }
    }

    /// Returns the validated path text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether the path is absolute in the guest namespace.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.0.starts_with('/')
    }

    /// Returns whether this path is equal to or beneath `ancestor`.
    #[must_use]
    pub fn is_within(&self, ancestor: &Self) -> bool {
        if !self.is_absolute() || !ancestor.is_absolute() {
            return false;
        }
        let prefix = ancestor.as_str();
        prefix == "/"
            || self.as_str() == prefix
            || self
                .as_str()
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

impl fmt::Display for GuestPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
