//! Toolchains — gcc, clang, go, rustc, make, cmake. Compile a small deterministic program INSIDE the
//! container and run it: the heaviest fork/exec/codegen path (driver → cc1/cc1plus → as → ld → exec),
//! the ultimate JIT stress. Plus `--version` banners. Both Linux arches. Owner: toolchains agent.
//! Recipes: docs/IMAGE-MANIFEST.md §5.
//!
//! AUTHORING LESSON (verified on the Real oracle): NEVER write a C/Go/Rust source file with shell
//! `printf '...%d...\n...'` — the SHELL's printf eats the `%d`/`\n` and corrupts the source (the old
//! seed produced `printf("sum=0<newline>",s)`). Always write sources via a QUOTED heredoc
//! (`cat > /m.c <<'EOF' … EOF`) so nothing is interpreted. The harness wraps each `.exec` script in an
//! outer `<<'DDEOF'` heredoc, so a nested `<<'EOF'` here is passed verbatim to the inner shell.
//!
//! XFAIL POLICY (GAPS.md) — post-#333 triage (the "exec-loader-noent" gap was a MISDIAGNOSIS: the ELF
//! loader / exec-of-child path is NOT broken; verified end-to-end on dd for go, clang, and rust/gcc-on-arm):
//!   * gcc/g++/cc COMPILE cases → `.xfail(AmdLinux)`: the x86_64 gcc DRIVER deterministically ICEs in
//!     `set_static_spec` (gcc.cc) under the JIT — the sole remaining engine bug (#240-class). ARM passes.
//!   * rust COMPILE cases → `.xfail(AmdLinux)`: rustc/LLVM codegen is fine on both arches, but the LINK
//!     step shells out to `cc` (gcc) → same x86 gcc-driver ICE on amd. ARM passes.
//!   * pinned gcc `--version` banners → `.xfail(AmdLinux)`: the driver ICEs before printing --version on x86.
//!   * `gcc:latest` banner → `.xfail(both)`: gcc-image-rootfs-leak (rootfs not isolated for that image).
//!   * go (compile+run+version), clang (compile+run), rustc/cargo `--version` → NOT xfailed (pass both
//!     arches). NOTE: go/rust need the image's Config.Env (PATH) present — a fresh `--long` pull records it;
//!     old pre-seeded fixtures that stripped it must be re-pulled (that env-loss was the real failure, not exec).
//! All cases pass on the Real oracle — xfail only gates the Dd backend.

use crate::scenario::{scen, sgroup, Scenario, ScenGroup, Target};

const BOTH: [Target; 2] = [Target::ArmLinux, Target::AmdLinux];
// exec-loader-noent (#333) triage: the ELF loader/exec-of-child path is NOT broken. Verified on dd:
// go (both arches), clang (both arches) and rust/gcc on ARM all compile+link+run correctly once the
// image's Config.Env (PATH) is present. The ONLY residual engine defect is a DETERMINISTIC x86 gcc
// driver ICE ("internal compiler error: in set_static_spec, at gcc.cc") that fires on every glibc/musl
// gcc invocation under the x86_64 JIT (gcc/g++/cc --version AND compile), and by extension any x86 link
// that shells out to gcc (rust on amd links via `cc`). So gcc/rust COMPILE cases stay xfail on AmdLinux
// only; go/clang/rust-version are no longer xfailed. (#240-class; tracked separately.)
const AMD: [Target; 1] = [Target::AmdLinux];

// ---- inline deterministic programs (written via quoted heredoc; markers are exact) ---------------

const C_SUM: &str = "#include <stdio.h>\nint main(void){ long s=0; for(long i=1;i<=1000;i++) s+=i; printf(\"R=%ld\\n\", s); return 0; }";
const C_FIB: &str = "#include <stdio.h>\nint main(void){ unsigned long long a=0,b=1; for(int i=0;i<50;i++){unsigned long long t=a+b;a=b;b=t;} printf(\"R=%llu\\n\",a); return 0; }";
const CPP_STL: &str = "#include <iostream>\n#include <numeric>\n#include <vector>\nint main(){ std::vector<long> v(1000); std::iota(v.begin(),v.end(),1); std::cout << \"R=\" << std::accumulate(v.begin(),v.end(),0L) << \"\\n\"; }";
const C_MAKE: &str = "#include <stdio.h>\nint main(void){ long s=0; for(long i=1;i<=1000;i++) s+=i; printf(\"make-ran-%ld\\n\", s); return 0; }";
const GO_SUM: &str = "package main\nimport \"fmt\"\nfunc main(){ s:=0; for i:=1;i<=1000;i++{ s+=i }; fmt.Printf(\"R=%d\\n\", s) }";
const GO_FIB: &str = "package main\nimport \"fmt\"\nfunc main(){ var a,b uint64 =0,1; for i:=0;i<50;i++{ a,b=b,a+b }; fmt.Printf(\"R=%d\\n\", a) }";
const RS_SUM: &str = "fn main(){ let s:u64=(1..=1000).sum(); println!(\"R={}\",s); }";
const RS_FIB: &str = "fn main(){ let (mut a,mut b):(u64,u64)=(0,1); for _ in 0..50 {let t=a+b;a=b;b=t;} println!(\"R={}\",a); }";

/// `cat > path <<'EOF' … EOF` — write a source file with ZERO shell interpretation (the only safe way).
fn hd(path: &str, body: &str) -> String {
    format!("cat > {path} <<'EOF'\n{body}\nEOF\n")
}

pub fn group() -> ScenGroup {
    let mut v: Vec<Scenario> = Vec::new();

    // ============================ COMPILE-AND-RUN (xfail AmdLinux — x86 gcc-driver ICE only) ======
    // -- alpine + build-base (gcc/musl) — small images, keep in quick class ------------------------
    // gcc/g++ COMPILE works on ARM (verified); on x86 the gcc driver ICEs in set_static_spec → AMD-only.
    v.push(scen("toolchains/alpine-cc-sum", "alpine")
        .exec(&format!("apk add --no-cache build-base >/dev/null 2>&1 || true\n{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
        .has("R=500500").timeout(180).xfail(&AMD));
    v.push(scen("toolchains/alpine-cc-fib", "alpine")
        .exec(&format!("apk add --no-cache build-base >/dev/null 2>&1 || true\n{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_FIB)))
        .has("R=12586269025").timeout(180).xfail(&AMD));
    v.push(scen("toolchains/alpine-gxx-stl", "alpine")
        .exec(&format!("apk add --no-cache build-base >/dev/null 2>&1 || true\n{}g++ -O2 /m.cpp -o /m && /m", hd("/m.cpp", CPP_STL)))
        .has("R=500500").timeout(180).xfail(&AMD));
    // cmake configure+build+run — heaviest fork/exec graph in the manifest.
    v.push(scen("toolchains/alpine-cmake-c", "alpine")
        .exec(&format!(
            "apk add --no-cache build-base cmake >/dev/null 2>&1 || true\nmkdir -p /p && cd /p\n{}{}cmake -S . -B b >/dev/null 2>&1 && cmake --build b >/dev/null 2>&1 && ./b/m",
            hd("m.c", C_SUM),
            hd("CMakeLists.txt", "cmake_minimum_required(VERSION 3.10)\nproject(dd C)\nadd_executable(m m.c)")))
        .has("R=500500").timeout(180).xfail(&AMD));

    // -- gcc:* (glibc driver → cc1/cc1plus/as/ld) — big images, long class -------------------------
    v.push(scen("toolchains/gcc-latest-c-sum", "gcc:latest")
        .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-latest-fib", "gcc:latest")
        .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_FIB)))
        .has("R=12586269025").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-14-c-sum", "gcc:14")
        .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-14-cpp-stl", "gcc:14")
        .exec(&format!("{}g++ -O2 /m.cpp -o /m && /m", hd("/m.cpp", CPP_STL)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-13-c-sum", "gcc:13")
        .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-13-cpp-stl", "gcc:13")
        .exec(&format!("{}g++ -O2 /m.cpp -o /m && /m", hd("/m.cpp", CPP_STL)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-12-cpp-stl", "gcc:12")
        .exec(&format!("{}g++ -O2 /m.cpp -o /m && /m", hd("/m.cpp", CPP_STL)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    // make orchestrates the compile+run (Makefile recipe lines need real leading TABs).
    v.push(scen("toolchains/gcc-make", "gcc:latest")
        .exec(&format!(
            "mkdir -p /p && cd /p\n{}{}make -s run",
            hd("m.c", C_MAKE),
            hd("Makefile", "run: m\n\t@./m\nm: m.c\n\tcc -O2 m.c -o m")))
        .has("make-ran-500500").timeout(180).long().xfail(&AMD));

    // -- clang / LLVM (glibc) — LLVM integrated toolchain (no gcc driver) works on BOTH arches (verified).
    v.push(scen("toolchains/clang-18-c-sum", "silkeh/clang:18")
        .exec(&format!("{}clang -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
        .has("R=500500").timeout(180).long());
    v.push(scen("toolchains/clang-18-cpp-stl", "silkeh/clang:18")
        .exec(&format!("{}clang++ -O2 /m.cpp -o /m && /m", hd("/m.cpp", CPP_STL)))
        .has("R=500500").timeout(180).long());
    v.push(scen("toolchains/clang-17-c-sum", "silkeh/clang:17")
        .exec(&format!("{}clang -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
        .has("R=500500").timeout(180).long());

    // -- go build/run (codegen + internal linker) — works on BOTH arches once the image PATH is present.
    let go_env = "export GOCACHE=/tmp/gocache GOFLAGS=-mod=mod\n";
    v.push(scen("toolchains/go-123-run-sum", "golang:1.23")
        .exec(&format!("{}{}go run /m.go", go_env, hd("/m.go", GO_SUM)))
        .has("R=500500").timeout(180).long());
    v.push(scen("toolchains/go-122-alpine-fib", "golang:1.22-alpine")
        .exec(&format!("{}{}go run /m.go", go_env, hd("/m.go", GO_FIB)))
        .has("R=12586269025").timeout(180).long());
    v.push(scen("toolchains/go-122-bookworm-build", "golang:1.22-bookworm")
        .exec(&format!("{}{}go build -o /m /m.go && /m", go_env, hd("/m.go", GO_SUM)))
        .has("R=500500").timeout(180).long());
    v.push(scen("toolchains/go-121-alpine-sum", "golang:1.21-alpine")
        .exec(&format!("{}{}go run /m.go", go_env, hd("/m.go", GO_SUM)))
        .has("R=500500").timeout(180).long());

    // -- rustc — rustc/LLVM codegen works on both arches, but the LINK step shells out to `cc` (gcc);
    // on x86 that gcc driver ICEs (set_static_spec) → rust COMPILE stays xfail on AmdLinux only.
    v.push(scen("toolchains/rust-179-slim-sum", "rust:1.79-slim")
        .exec(&format!("{}rustc -O /m.rs -o /m && /m", hd("/m.rs", RS_SUM)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/rust-179-sum", "rust:1.79")
        .exec(&format!("{}rustc -O /m.rs -o /m && /m", hd("/m.rs", RS_SUM)))
        .has("R=500500").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/rust-178-slim-fib", "rust:1.78-slim")
        .exec(&format!("{}rustc -O /m.rs -o /m && /m", hd("/m.rs", RS_FIB)))
        .has("R=12586269025").timeout(180).long().xfail(&AMD));
    v.push(scen("toolchains/rust-178-alpine-fib", "rust:1.78-alpine")
        .exec(&format!("{}rustc -O /m.rs -o /m && /m", hd("/m.rs", RS_FIB)))
        .has("R=12586269025").timeout(180).long().xfail(&AMD));

    // ============================ VERSION BANNERS =================================================
    // gcc:latest banners → xfail both (gcc-image-rootfs-leak: rootfs not isolated for this image).
    v.push(scen("toolchains/gcc-latest-banner", "gcc:latest")
        .exec("gcc --version | head -1").has("gcc (GCC)").timeout(120).long().xfail(&BOTH));
    v.push(scen("toolchains/gcc-latest-make-banner", "gcc:latest")
        .exec("make --version | head -1").has("GNU Make").timeout(120).long().xfail(&[Target::AmdLinux]));
    v.push(scen("toolchains/gcc-latest-ld-banner", "gcc:latest")
        .exec("ld --version | head -1").has("GNU ld").timeout(120).long().xfail(&[Target::AmdLinux]));
    v.push(scen("toolchains/gcc-latest-as-banner", "gcc:latest")
        .exec("as --version | head -1").has("GNU assembler").timeout(120).long().xfail(&[Target::AmdLinux]));

    // pinned gcc banners — ARM prints the banner fine; on x86 the gcc DRIVER itself ICEs in
    // set_static_spec before printing --version (deterministic) → xfail AmdLinux (#333/#240 x86 gcc bug).
    v.push(scen("toolchains/gcc-14-banner", "gcc:14")
        .exec("gcc --version | head -1").has("gcc (GCC) 14").timeout(120).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-13-banner", "gcc:13")
        .exec("gcc --version | head -1").has("gcc (GCC) 13").timeout(120).long().xfail(&AMD));
    v.push(scen("toolchains/gcc-12-gpp-banner", "gcc:12")
        .exec("g++ --version | head -1").has("g++ (GCC) 12").timeout(120).long().xfail(&AMD));

    // clang/LLVM banners — no documented gap (not xfailed). silkeh/clang prints "Debian clang version 18.x".
    v.push(scen("toolchains/clang-18-banner", "silkeh/clang:18")
        .exec("clang --version | head -1").has("clang version 18").timeout(120).long());
    v.push(scen("toolchains/clang-17-banner", "silkeh/clang:17")
        .exec("clang --version | head -1").has("clang version 17").timeout(120).long());
    v.push(scen("toolchains/clang-18-llvm-config", "silkeh/clang:18")
        .exec("llvm-config --version").has("18.1").timeout(120).long());

    // go banners → work on BOTH arches (go binary exec's + runs fine once the image PATH is present).
    v.push(scen("toolchains/go-123-banner", "golang:1.23")
        .exec("go version").has("go1.23").timeout(120).long());
    v.push(scen("toolchains/go-121-alpine-banner", "golang:1.21-alpine")
        .exec("go version").has("go1.21").timeout(120).long());

    // rust banners → work on BOTH arches (rustc/cargo --version does not link, so no gcc dependency).
    v.push(scen("toolchains/rust-178-slim-banner", "rust:1.78-slim")
        .exec("rustc --version").has("rustc 1.78").timeout(120).long());
    v.push(scen("toolchains/rust-179-cargo-banner", "rust:1.79")
        .exec("cargo --version").has("cargo 1.79").timeout(120).long());

    // ============================ SANITY: base images carry NO compiler ===========================
    // Pure shell (no fork/exec of a toolchain binary) → should pass on dd; not xfailed.
    v.push(scen("toolchains/ubuntu-no-cc", "ubuntu:24.04")
        .exec("command -v gcc || echo NO-CC").has("NO-CC"));
    v.push(scen("toolchains/debian-no-cc", "debian:bookworm")
        .exec("command -v cc || echo NO-CC").has("NO-CC"));
    v.push(scen("toolchains/alpine-no-cc", "alpine:latest")
        .exec("command -v gcc || echo NO-CC").has("NO-CC"));

    sgroup("toolchains", v)
}
