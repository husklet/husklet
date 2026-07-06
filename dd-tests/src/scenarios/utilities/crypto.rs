//! crypto / digests — sha256/md5/openssl/base64 over published vectors.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- crypto / digests --------------------------------------------------------------------
        // sha256("abc") = published NIST vector. busybox/coreutils sha256 applet (musl).
        scen("utilities/sha256-abc", "alpine")
            .exec("printf abc | sha256sum | cut -d' ' -f1")
            .has("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        // same vector through glibc coreutils — distinct libc/codegen path.
        scen("utilities/sha256-abc-glibc", "debian:bookworm")
            .exec("printf abc | sha256sum | cut -d' ' -f1")
            .has("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        // md5("abc") = published vector.
        scen("utilities/md5-abc", "alpine")
            .exec("printf abc | md5sum | cut -d' ' -f1")
            .has("900150983cd24fb0d6963f7d28e17f72"),
        // real OpenSSL EVP digest — entrypoint=openssl, so run-form; empty stdin → sha256("") vector.
        scen("utilities/openssl-sha256-empty", "alpine/openssl:latest")
            .run(&["dgst", "-sha256", "-r"])
            .has("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        scen("utilities/openssl-version", "alpine/openssl:latest")
            .run(&["version"])
            .has("OpenSSL 3"),
        // base64 encode / round-trip (deterministic).
        scen("utilities/base64-encode", "alpine")
            .exec("printf dd | base64")
            .has("ZGQ="),
        scen("utilities/base64-roundtrip", "alpine")
            .exec("printf hello | base64 | base64 -d")
            .has("hello"),
    ]
}
