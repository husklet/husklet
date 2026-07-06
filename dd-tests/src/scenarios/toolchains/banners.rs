//! `--version` banners for every toolchain (gcc/make/ld/as, pinned gcc, clang/llvm-config, go, rust).
//! No compile/link — just exec the binary and match its banner. All pass on both arches, no xfail.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();

    // gcc:latest banners — pass on both arches (verified).
    v.push(
        scen("toolchains/gcc-latest-banner", "gcc:latest")
            .exec("gcc --version | head -1")
            .has("gcc (GCC)")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-latest-make-banner", "gcc:latest")
            .exec("make --version | head -1")
            .has("GNU Make")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-latest-ld-banner", "gcc:latest")
            .exec("ld --version | head -1")
            .has("GNU ld")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-latest-as-banner", "gcc:latest")
            .exec("as --version | head -1")
            .has("GNU assembler")
            .timeout(120)
            .long(),
    );

    // pinned gcc banners — pass on both arches (the x86 gcc-driver set_static_spec ICE is fixed).
    v.push(
        scen("toolchains/gcc-14-banner", "gcc:14")
            .exec("gcc --version | head -1")
            .has("gcc (GCC) 14")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-13-banner", "gcc:13")
            .exec("gcc --version | head -1")
            .has("gcc (GCC) 13")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/gcc-12-gpp-banner", "gcc:12")
            .exec("g++ --version | head -1")
            .has("g++ (GCC) 12")
            .timeout(120)
            .long(),
    );

    // clang/LLVM banners — no documented gap (not xfailed). silkeh/clang prints "Debian clang version 18.x".
    v.push(
        scen("toolchains/clang-18-banner", "silkeh/clang:18")
            .exec("clang --version | head -1")
            .has("clang version 18")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/clang-17-banner", "silkeh/clang:17")
            .exec("clang --version | head -1")
            .has("clang version 17")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/clang-18-llvm-config", "silkeh/clang:18")
            .exec("llvm-config --version")
            .has("18.1")
            .timeout(120)
            .long(),
    );

    // go banners → work on BOTH arches (go binary exec's + runs fine once the image PATH is present).
    v.push(
        scen("toolchains/go-123-banner", "golang:1.23")
            .exec("go version")
            .has("go1.23")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/go-121-alpine-banner", "golang:1.21-alpine")
            .exec("go version")
            .has("go1.21")
            .timeout(120)
            .long(),
    );

    // rust banners → work on BOTH arches (rustc/cargo --version does not link, so no gcc dependency).
    v.push(
        scen("toolchains/rust-178-slim-banner", "rust:1.78-slim")
            .exec("rustc --version")
            .has("rustc 1.78")
            .timeout(120)
            .long(),
    );
    v.push(
        scen("toolchains/rust-179-cargo-banner", "rust:1.79")
            .exec("cargo --version")
            .has("cargo 1.79")
            .timeout(120)
            .long(),
    );

    v
}
