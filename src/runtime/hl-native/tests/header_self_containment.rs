use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn headers(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("native header directory") {
        let path = entry.expect("native header entry").path();
        if path.is_dir() {
            headers(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "h") {
            output.push(path);
        }
    }
}

#[test]
fn owned_native_headers_are_self_contained() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let guest = native.join("translator/guest/x86_64");
    let mut owned = Vec::new();
    headers(&guest, &mut owned);
    for boundary in ["bridge", "engine", "host/linux", "host/macos", "include", "linux_abi"] {
        headers(&native.join(boundary), &mut owned);
    }
    if cfg!(target_os = "windows") {
        headers(&native.join("host/windows"), &mut owned);
    } else {
        // These shared Windows boundaries are deliberately empty or Win32-type-free off Windows. The
        // fault and internal headers require the platform SDK and join the sweep on a Windows host.
        owned.extend([
            native.join("host/windows/launch.h"),
            native.join("host/windows/win32.h"),
        ]);
    }
    owned.sort();

    // These files explicitly document that they are implementation/composition fragments expanded only
    // after their target translation unit has established the target-specific macros and private helpers.
    owned.retain(|path| {
        let relative = path.strip_prefix(&native).expect("native header");
        let relative = relative.to_string_lossy();
        !matches!(
            relative.as_ref(),
            "translator/guest/x86_64/dispatch.h"
                | "translator/guest/x86_64/interp_dispatch.h"
                | "linux_abi/elf_protect.h"
                | "linux_abi/guest_stat.h"
                | "linux_abi/syscall/nonpie_args.h"
                | "linux_abi/syscall/sysv_state.h"
        )
    });

    let scratch = std::env::temp_dir().join(format!("hl-native-header-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("header probe directory");
    let probe = scratch.join("probe.c");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    for header in owned {
        let relative = header.strip_prefix(&native).expect("owned native header");
        fs::write(&probe, format!("#include \"{}\"\n", relative.display())).expect("header probe source");
        let result = Command::new(&compiler)
            .args([
                "-std=c11",
                "-D_GNU_SOURCE",
                "-Werror=implicit-function-declaration",
                "-fsyntax-only",
            ])
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
