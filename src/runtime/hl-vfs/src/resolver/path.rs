use std::collections::VecDeque;

use crate::{GuestName, GuestPathBytes, PathError, ResolveError};

const COMPONENT_MAXIMUM: usize = 255;
const RESOLUTION_COMPONENT_MAXIMUM: usize = 512;
const RESOLUTION_PATH_MAXIMUM: usize = 4096;

pub(crate) struct PreparedPath {
    pub(crate) components: VecDeque<Component>,
    pub(crate) base_depth: usize,
}

impl PreparedPath {
    pub(crate) fn new_with_root(
        path: &GuestPathBytes,
        base: &GuestPathBytes,
        in_root: bool,
    ) -> Result<Self, ResolveError> {
        if path.as_bytes().is_empty() {
            return Err(ResolveError::Path(PathError::Empty));
        }
        if !base.is_absolute() {
            return Err(ResolveError::RelativeBase);
        }
        let rooted = path.is_absolute() && in_root;
        let combined = if path.is_absolute() && !rooted {
            path.as_bytes().to_vec()
        } else if base.as_bytes() == b"/" {
            let mut combined = Vec::with_capacity(path.as_bytes().len() + 1);
            combined.push(b'/');
            combined.extend_from_slice(path.as_bytes().strip_prefix(b"/").unwrap_or(path.as_bytes()));
            combined
        } else {
            let capacity = base
                .as_bytes()
                .len()
                .checked_add(path.as_bytes().len())
                .and_then(|length| length.checked_add(1))
                .ok_or(ResolveError::PathTooLong)?;
            if capacity >= RESOLUTION_PATH_MAXIMUM {
                return Err(ResolveError::PathTooLong);
            }
            let mut combined = Vec::with_capacity(capacity);
            combined.extend_from_slice(base.as_bytes());
            combined.push(b'/');
            combined.extend_from_slice(path.as_bytes().strip_prefix(b"/").unwrap_or(path.as_bytes()));
            combined
        };
        let mut prepared = Self::from_bytes(&combined)?;
        prepared.base_depth = if path.is_absolute() && !rooted {
            0
        } else {
            Self::from_bytes(base.as_bytes())?.components.len()
        };
        Ok(prepared)
    }

    pub(crate) fn from_bytes(path: &[u8]) -> Result<Self, ResolveError> {
        if path.len() >= RESOLUTION_PATH_MAXIMUM {
            return Err(ResolveError::PathTooLong);
        }
        let mut components = VecDeque::new();
        for component in path.split(|byte| *byte == b'/') {
            if component.is_empty() {
                continue;
            }
            if component.len() > COMPONENT_MAXIMUM {
                return Err(ResolveError::ComponentTooLong);
            }
            if components.len() == RESOLUTION_COMPONENT_MAXIMUM {
                return Err(ResolveError::TooManyComponents);
            }
            components.push_back(Component::new(component)?);
        }
        Ok(Self {
            components,
            base_depth: 0,
        })
    }
}

pub(crate) enum Component {
    Current,
    Parent,
    Name(GuestName),
}

impl Component {
    fn new(bytes: &[u8]) -> Result<Self, ResolveError> {
        match bytes {
            b"." => Ok(Self::Current),
            b".." => Ok(Self::Parent),
            name => GuestName::new(name).map(Self::Name).map_err(ResolveError::Path),
        }
    }

    pub(crate) fn byte_length(&self) -> usize {
        match self {
            Self::Current => 1,
            Self::Parent => 2,
            Self::Name(name) => name.as_bytes().len(),
        }
    }
}
