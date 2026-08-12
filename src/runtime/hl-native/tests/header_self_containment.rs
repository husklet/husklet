use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn headers(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("translator header directory") {
        let path = entry.expect("translator header entry").path();
        if path.is_dir() {
            headers(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "h") {
            output.push(path);
        }
    }
}

#[test]
fn x86_translator_owned_headers_are_self_contained() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let guest = native.join("translator/guest/x86_64");
    let mut owned = Vec::new();
    headers(&guest, &mut owned);
    owned.sort();

    // These two files are macro composition fragments expanded inside the complete target TU.
    owned.retain(|path| {
        !matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("dispatch.h" | "interp_dispatch.h")
        )
    });

    let scratch = std::env::temp_dir().join(format!("hl-native-header-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("header probe directory");
    let probe = scratch.join("probe.c");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    for header in owned {
        let relative = header.strip_prefix(&guest).expect("x86 translator header");
        fs::write(&probe, format!("#include \"{}\"\n", relative.display())).expect("header probe source");
        let result = Command::new(&compiler)
            .args(["-std=c11", "-fsyntax-only"])
            .arg(format!("-I{}", guest.display()))
            .arg(format!("-I{}", native.display()))
            .arg(format!("-I{}", native.join("include").display()))
            .arg(&probe)
            .output()
            .expect("C compiler for header probe");
        assert!(
            result.status.success(),
            "{} is not a self-contained first include:\n{}",
            header.display(),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::remove_dir_all(scratch).expect("remove header probe directory");
}
