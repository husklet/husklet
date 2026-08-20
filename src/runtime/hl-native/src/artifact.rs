//! Portable identity and loader locations for the private native artifact.

pub(crate) const fn filename(target_os: &str) -> &'static str {
    match target_os.as_bytes() {
        b"macos" => "libhl_native_engine.dylib",
        b"windows" => "hl_native_engine.dll",
        _ => "libhl_native_engine.so",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_artifacts_have_native_names() {
        assert_eq!(super::filename("linux"), "libhl_native_engine.so");
        assert_eq!(super::filename("macos"), "libhl_native_engine.dylib");
        assert_eq!(super::filename("windows"), "hl_native_engine.dll");
    }
}
