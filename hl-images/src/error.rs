//! The typed error for `dd-images`, replacing the crate's former stringly-typed `Result<_, String>`
//! public surface (mirrors [`hl_jit::Error`](../../hl_jit/enum.Error.html)).
//!
//! Every variant except [`Error::Io`] simply CARRIES the message string a call site produced, and
//! [`Display`](std::fmt::Display) writes that string back VERBATIM. This is deliberate: the daemon
//! surfaces some of these errors as HTTP `{"message": …}` bodies, so the categorization is purely for
//! callers that want to match on a class of failure — it never rewrites the wire text.

use std::fmt;

/// An error from image handling: pulling/pushing from a registry, parsing a manifest/config, or
/// saving/loading/importing an archive.
///
/// The message-carrying variants reproduce their payload byte-for-byte through
/// [`Display`](std::fmt::Display); the category only classifies the failure and never alters the text.
#[derive(Debug)]
pub enum Error {
    /// An underlying I/O failure (carries the [`std::io::Error`]); its [`Display`](std::fmt::Display)
    /// is the inner error's own message.
    Io(std::io::Error),
    /// A network / registry / HTTP failure (curl transport, auth, non-2xx responses).
    Registry(String),
    /// A manifest or image-config parse failure (bad/absent JSON, missing fields).
    Manifest(String),
    /// A `tar` / save / load / import archive failure.
    Archive(String),
    /// A sha256 digest failure (hashing, decompression, or format).
    Digest(String),
    /// A missing image, blob, tag, or platform variant.
    NotFound(String),
    /// Any failure not covered by a more specific category.
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The inner io error's Display — matches a bare `e.to_string()` at a call site.
            Error::Io(e) => write!(f, "{e}"),
            // Every categorized variant writes its carried message VERBATIM (byte-for-byte): this is
            // what keeps the daemon's HTTP `{"message": …}` bodies identical to the old String surface.
            Error::Registry(m)
            | Error::Manifest(m)
            | Error::Archive(m)
            | Error::Digest(m)
            | Error::NotFound(m)
            | Error::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The whole point of the refactor: a categorized variant's Display is its carried message,
    // byte-for-byte. If this ever regresses, the daemon's HTTP error bodies drift.
    #[test]
    fn string_variants_display_verbatim() {
        // A message with punctuation/interpolation-shaped text that must survive untouched.
        let msg = "tar extract failed: Can't unlink already-existing object: Permission denied";
        assert_eq!(Error::Registry(msg.to_string()).to_string(), msg);
        assert_eq!(Error::Manifest(msg.to_string()).to_string(), msg);
        assert_eq!(Error::Archive(msg.to_string()).to_string(), msg);
        assert_eq!(Error::Digest(msg.to_string()).to_string(), msg);
        assert_eq!(Error::NotFound(msg.to_string()).to_string(), msg);
        assert_eq!(Error::Other(msg.to_string()).to_string(), msg);
        // The empty string round-trips too (no prefix/suffix is ever added).
        assert_eq!(Error::Other(String::new()).to_string(), "");
    }

    // `From<io::Error>` + Display shape: Display equals the inner io error's own message, and `source`
    // exposes the wrapped error.
    #[test]
    fn io_from_and_display_match_inner() {
        let make = || std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let expected = make().to_string();
        let err: Error = make().into();
        assert_eq!(err.to_string(), expected);
        assert!(std::error::Error::source(&err).is_some());
    }
}
