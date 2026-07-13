//! C/C++ compile-and-run: alpine build-base (gcc/musl) + cmake, the glibc `gcc:*` driver
//! (cc1/cc1plus/as/ld) incl. `make`, and the clang/LLVM integrated toolchain. Both arches, no xfail.

use crate::scenario::{scen, Scenario};
use super::{hd, C_SUM, C_FIB, CPP_STL, C_MAKE};

pub(super) fn items() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();

    // -- alpine + build-base (gcc/musl) — small images, keep in quick class ------------------------
    // gcc/g++/cc compile+link+run on BOTH arches (verified); the x86 gcc-driver set_static_spec ICE is fixed.
    v.push(
        scen("toolchains/alpine-cc-sum", "alpine")
            .exec(&format!(
                "apk add --no-cache build-base >/dev/null 2>&1 || true\n{}cc -O2 /m.c -o /m && /m",
                hd("/m.c", C_SUM)
            ))
            .has("R=500500")
            .timeout(180),
    );
    v.push(
        scen("toolchains/alpine-cc-fib", "alpine")
            .exec(&format!(
                "apk add --no-cache build-base >/dev/null 2>&1 || true\n{}cc -O2 /m.c -o /m && /m",
                hd("/m.c", C_FIB)
            ))
            .has("R=12586269025")
            .timeout(180),
    );
    v.push(scen("toolchains/alpine-gxx-stl", "alpine")
        .exec(&format!("apk add --no-cache build-base >/dev/null 2>&1 || true\n{}g++ -O2 /m.cpp -o /m && /m", hd("/m.cpp", CPP_STL)))
        .has("R=500500").timeout(180));
    // cmake configure+build+run — heaviest fork/exec graph in the manifest.
    v.push(scen("toolchains/alpine-cmake-c", "alpine")
        .exec(&format!(
            "apk add --no-cache build-base cmake >/dev/null 2>&1 || true\nmkdir -p /p && cd /p\n{}{}cmake -S . -B b >/dev/null 2>&1 && cmake --build b >/dev/null 2>&1 && ./b/m",
            hd("m.c", C_SUM),
            hd("CMakeLists.txt", "cmake_minimum_required(VERSION 3.10)\nproject(dd C)\nadd_executable(m m.c)")))
        .has("R=500500").timeout(180));

    // -- gcc:* (glibc driver → cc1/cc1plus/as/ld) — big images, long class -------------------------
    v.push(
        scen("toolchains/gcc-latest-c-sum", "gcc:latest")
            .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-latest-fib", "gcc:latest")
            .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_FIB)))
            .has("R=12586269025")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-14-c-sum", "gcc:14")
            .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-14-cpp-stl", "gcc:14")
            .exec(&format!(
                "{}g++ -O2 /m.cpp -o /m && /m",
                hd("/m.cpp", CPP_STL)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-13-c-sum", "gcc:13")
            .exec(&format!("{}cc -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-13-cpp-stl", "gcc:13")
            .exec(&format!(
                "{}g++ -O2 /m.cpp -o /m && /m",
                hd("/m.cpp", CPP_STL)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-12-cpp-stl", "gcc:12")
            .exec(&format!(
                "{}g++ -O2 /m.cpp -o /m && /m",
                hd("/m.cpp", CPP_STL)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    // make orchestrates the compile+run (Makefile recipe lines need real leading TABs).
    v.push(
        scen("toolchains/gcc-make", "gcc:latest")
            .exec(&format!(
                "mkdir -p /p && cd /p\n{}{}make -s run",
                hd("m.c", C_MAKE),
                hd("Makefile", "run: m\n\t@./m\nm: m.c\n\tcc -O2 m.c -o m")
            ))
            .has("make-ran-500500")
            .timeout(180)
            .long(),
    );

    // -- clang / LLVM (glibc) — LLVM integrated toolchain (no gcc driver) works on BOTH arches (verified).
    v.push(
        scen("toolchains/clang-18-c-sum", "silkeh/clang:18")
            .exec(&format!("{}clang -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/clang-18-cpp-stl", "silkeh/clang:18")
            .exec(&format!(
                "{}clang++ -O2 /m.cpp -o /m && /m",
                hd("/m.cpp", CPP_STL)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/clang-17-c-sum", "silkeh/clang:17")
            .exec(&format!("{}clang -O2 /m.c -o /m && /m", hd("/m.c", C_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );

    v
}
