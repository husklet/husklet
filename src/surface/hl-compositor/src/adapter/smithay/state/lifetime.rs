#[cfg(test)]
mod tests {
    use super::{surface_identity_version, TabletToolCapabilities, TABLET_TOOL_CAPABILITIES};

    #[test]
    fn tablet_advertises_only_axes_emitted_by_the_host_seam() {
        assert_eq!(TABLET_TOOL_CAPABILITIES, TabletToolCapabilities::PRESSURE);
    }

    #[test]
    fn native_identity_global_is_absent_without_native_frames() {
        assert_eq!(surface_identity_version(false), None);
        assert_eq!(
            surface_identity_version(true),
            Some(hl_surface_protocol::VERSION)
        );
    }
}
