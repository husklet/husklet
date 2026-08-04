pub(super) struct DescriptorLink;

impl DescriptorLink {
    pub(super) fn resolve(path: &[u8]) -> Option<i32> {
        let digits = path
            .strip_prefix(b"/proc/self/fd/")
            .or_else(|| path.strip_prefix(b"/dev/fd/"))?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    pub(super) fn resolve_at(base: &[u8], path: &[u8]) -> Option<i32> {
        if path.starts_with(b"/") {
            return Self::resolve(path);
        }
        if !matches!(
            base,
            b"/proc/self/fd" | b"/proc/self/fd/" | b"/dev/fd" | b"/dev/fd/"
        ) {
            return None;
        }
        if path.is_empty() || path.contains(&b'/') {
            return None;
        }
        Self::resolve(&[base.strip_suffix(b"/").unwrap_or(base), b"/", path].concat())
    }
}

#[cfg(test)]
mod tests {
    use super::DescriptorLink;

    #[test]
    fn parses_self_descriptor_links() {
        assert_eq!(DescriptorLink::resolve(b"/proc/self/fd/42"), Some(42));
        assert_eq!(DescriptorLink::resolve(b"/proc/self/fd/"), None);
        assert_eq!(DescriptorLink::resolve(b"/proc/self/fd/4x"), None);
        assert_eq!(DescriptorLink::resolve(b"/proc/1/fd/42"), None);
        assert_eq!(DescriptorLink::resolve(b"/dev/fd/42"), Some(42));
        assert_eq!(DescriptorLink::resolve_at(b"/proc/self/fd", b"42"), Some(42));
        assert_eq!(DescriptorLink::resolve_at(b"/tmp", b"42"), None);
    }
}
