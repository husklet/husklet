use std::{
    fs,
    io::Write as _,
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

#[test]
fn public_abi_is_self_contained_for_c_and_cpp_consumers() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let include = native.join("include");
    let scratch = std::env::temp_dir().join(format!("hl-native-public-abi-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("public ABI probe directory");
    let mut public = headers_in(&include.join("hl"));
    public.push(native.join("bridge/api.h"));
    public.sort();

    for (compiler, standard, extension) in [
        (std::env::var_os("CC").unwrap_or_else(|| "cc".into()), "c11", "c"),
        (std::env::var_os("CXX").unwrap_or_else(|| "c++".into()), "c++17", "cpp"),
    ] {
        for header in &public {
            // linux_abi exposes C11 atomic_flag as part of its concrete implementation state. Its API is C;
            // the portable bridge and all other public headers remain valid C++ boundaries.
            if extension == "cpp" && header.ends_with("linux_abi.h") {
                continue;
            }
            let relative = header.strip_prefix(&native).expect("public native header");
            let probe = scratch.join(format!("probe.{extension}"));
            fs::write(&probe, format!("#include \"{}\"\n", relative.display())).expect("public ABI probe source");
            let result = Command::new(&compiler)
                .arg(format!("-std={standard}"))
                .arg("-fsyntax-only")
                .arg(format!("-I{}", native.display()))
                .arg(format!("-I{}", include.display()))
                .arg(&probe)
                .output()
                .expect("C or C++ compiler for public ABI probe");
            assert!(
                result.status.success(),
                "{} is not self-contained under {standard}:\n{}",
                header.display(),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    fs::remove_dir_all(scratch).expect("remove public ABI probe directory");
}

fn headers_in(directory: &Path) -> Vec<PathBuf> {
    let mut output = Vec::new();
    headers(directory, &mut output);
    output
}

#[test]
fn public_visibility_selects_export_and_import_annotations() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let include = package.join("src/native/include");
    let compiler = std::env::var_os("CC").unwrap_or_else(|| "cc".into());
    let preprocess = |definitions: &[&str]| {
        let mut command = Command::new(&compiler);
        command.args(["-E", "-P", "-x", "c"]);
        for definition in definitions {
            command.arg(format!("-D{definition}"));
        }
        command.arg(format!("-I{}", include.display())).arg("-");
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("C preprocessor for visibility probe");
        child
            .stdin
            .take()
            .expect("visibility probe stdin")
            .write_all(b"#include \"hl/base.h\"\nHL_API void hl_visibility_probe(void);\n")
            .expect("visibility probe source");
        let output = child.wait_with_output().expect("visibility probe output");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("visibility probe UTF-8")
    };

    let posix = preprocess(&[]);
    assert!(posix.contains("visibility(\"default\")"));
    let windows_export = preprocess(&["_WIN32", "HL_SHARED", "HL_BUILDING_ENGINE"]);
    assert!(windows_export.contains("__declspec(dllexport)"));
    let windows_import = preprocess(&["_WIN32", "HL_SHARED"]);
    assert!(windows_import.contains("__declspec(dllimport)"));
}

#[cfg(target_os = "linux")]
#[test]
fn cpp_bridge_declarations_retain_c_linkage() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-cpp-linkage-probe-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("C++ linkage probe directory");
    let source = scratch.join("probe.cpp");
    let object = scratch.join("probe.o");
    fs::write(
        &source,
        "#include \"bridge/api.h\"\nvoid probe() { hl_c_backend_destroy(nullptr); }\n",
    )
    .expect("C++ linkage probe source");
    let compile = Command::new(std::env::var_os("CXX").unwrap_or_else(|| "c++".into()))
        .args(["-std=c++17", "-c"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .output()
        .expect("C++ linkage probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let symbols = Command::new("nm")
        .arg("-u")
        .arg(&object)
        .output()
        .expect("nm for C++ linkage probe");
    assert!(symbols.status.success());
    let symbols = String::from_utf8(symbols.stdout).expect("nm UTF-8");
    assert!(
        symbols.lines().any(|line| line.ends_with(" U hl_c_backend_destroy")),
        "{symbols}"
    );
    fs::remove_dir_all(scratch).expect("remove C++ linkage probe directory");
}
