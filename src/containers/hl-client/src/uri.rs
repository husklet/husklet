use std::fmt::{self, Display, Formatter};

use percent_encoding::{AsciiSet, CONTROLS, NON_ALPHANUMERIC, utf8_percent_encode};

const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// One value encoded for use inside a Docker HTTP URI.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Component<'a> {
    value: &'a str,
    set: &'static AsciiSet,
}

impl<'a> Component<'a> {
    /// Treat a Docker identifier or query value as opaque data.
    pub(crate) const fn opaque(value: &'a str) -> Self {
        Self {
            value,
            set: NON_ALPHANUMERIC,
        }
    }

    /// Preserve characters permitted directly in an RFC URI path segment.
    pub(crate) const fn segment(value: &'a str) -> Self {
        Self { value, set: SEGMENT }
    }
}

impl Display for Component<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        utf8_percent_encode(self.value, self.set).fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::Component;

    #[test]
    fn policies_preserve_existing_docker_uri_contracts() {
        assert_eq!(Component::opaque("name/v1-test").to_string(), "name%2Fv1%2Dtest");
        assert_eq!(Component::segment("name/v1-test").to_string(), "name%2Fv1-test");
    }
}
