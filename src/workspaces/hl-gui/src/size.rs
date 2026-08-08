use std::fmt;

/// A non-negative byte count rendered in compact binary units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteSize(i64);

impl ByteSize {
    #[must_use]
    pub fn new(bytes: i64) -> Self {
        Self(bytes.max(0))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0 as f64;
        if bytes < 1024.0 {
            write!(formatter, "{} B", self.0)
        } else if bytes < 1024.0 * 1024.0 {
            write!(formatter, "{:.0} KB", bytes / 1024.0)
        } else if bytes < 1024.0 * 1024.0 * 1024.0 {
            write!(formatter, "{:.1} MB", bytes / (1024.0 * 1024.0))
        } else {
            write!(formatter, "{:.1} GB", bytes / (1024.0 * 1024.0 * 1024.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_uses_compact_binary_units() {
        assert_eq!(ByteSize::new(-5).to_string(), "0 B");
        assert_eq!(ByteSize::new(512).to_string(), "512 B");
        assert_eq!(ByteSize::new(1024).to_string(), "1 KB");
        assert_eq!(ByteSize::new(1536).to_string(), "2 KB");
        assert_eq!(ByteSize::new(1024 * 1024).to_string(), "1.0 MB");
        assert_eq!(ByteSize::new(1024 * 1024 * 3 / 2).to_string(), "1.5 MB");
        assert_eq!(ByteSize::new(1024 * 1024 * 1024).to_string(), "1.0 GB");
    }
}
