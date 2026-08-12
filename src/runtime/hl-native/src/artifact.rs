//! Portable identity and loader locations for the private native artifact.

pub(crate) const fn filename(target_os: &str) -> &'static str {
    match target_os.as_bytes() {
        b"macos" => "libhl_native_engine.dylib",
        b"windows" => "hl_native_engine.dll",
        _ => "libhl_native_engine.so",
    }
}

pub(crate) const fn loader_paths(target_os: &str) -> &'static [&'static str] {
    match target_os.as_bytes() {
        // bundle.sh relocates private dylibs into Contents/Frameworks. The
        // loader-relative entry also supports a library-local test layout.
        b"macos" => &["@executable_path/../Frameworks", "@loader_path"],
        // Windows searches the executable directory for private DLLs.
        b"windows" => &[],
        // Nix and conventional Unix packages place private libraries in lib.
        _ => &["$ORIGIN/../lib", "$ORIGIN"],
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn platform_artifacts_have_native_names_and_relocatable_loader_paths() {
        assert_eq!(super::filename("linux"), "libhl_native_engine.so");
        assert_eq!(super::filename("macos"), "libhl_native_engine.dylib");
        assert_eq!(super::filename("windows"), "hl_native_engine.dll");
        assert_eq!(super::loader_paths("linux"), &["$ORIGIN/../lib", "$ORIGIN"]);
        assert_eq!(
            super::loader_paths("macos"),
            &["@executable_path/../Frameworks", "@loader_path"]
        );
        assert!(super::loader_paths("windows").is_empty());
    }
}
