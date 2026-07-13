//! Compression / crypto codegen: gzip/bzip2/xz/zstd roundtrips (deflate / Burrows-Wheeler / LZMA /
//! zstd asm) and OpenSSL SHA-256/SHA-512/AES (SHA-NI / armv8 crypto-extension asm). Shell-driven,
//! no compiler — the value must survive compress+decompress / match the NIST vector.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // gzip (zlib deflate) roundtrip — the value survives compress+decompress.
        scen("weird/gzip-roundtrip", "alpine:latest")
            .exec("seq 1 1000 | gzip -9 | gzip -d | awk '{s+=$1}END{print \"GZIP=\"s}'")
            .has("GZIP=500500"),
        // bzip2 (Burrows-Wheeler) roundtrip.
        scen("weird/bzip2-roundtrip", "alpine:latest")
            .exec("seq 1 1000 | bzip2 -9 | bzip2 -d | awk '{s+=$1}END{print \"BZIP=\"s}'")
            .has("BZIP=500500"),
        // xz (LZMA) roundtrip.
        scen("weird/xz-roundtrip", "debian:bookworm")
            .exec("apt-get update >/dev/null 2>&1 && apt-get install -y xz-utils >/dev/null 2>&1; seq 1 1000 | xz -9 | xz -d | awk '{s+=$1}END{print \"XZ=\"s}'")
            .has("XZ=500500").long(),
        // zstd roundtrip.
        scen("weird/zstd-roundtrip", "debian:bookworm")
            .exec("apt-get update >/dev/null 2>&1 && apt-get install -y zstd >/dev/null 2>&1; seq 1 1000 | zstd -19 2>/dev/null | zstd -d 2>/dev/null | awk '{s+=$1}END{print \"ZSTD=\"s}'")
            .has("ZSTD=500500").long(),
        // OpenSSL SHA-256 NIST vector — libcrypto SHA-NI / armv8 crypto-extension asm.
        scen("weird/openssl-sha256", "alpine:latest")
            .exec("apk add --no-cache openssl >/dev/null 2>&1; printf abc | openssl dgst -sha256 | awk '{print $NF}'")
            .has("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").long(),
        // OpenSSL SHA-512 NIST vector.
        scen("weird/openssl-sha512", "alpine:latest")
            .exec("apk add --no-cache openssl >/dev/null 2>&1; printf abc | openssl dgst -sha512 | awk '{print $NF}'")
            .has("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f").long(),
        // OpenSSL speed: a bounded tight AES asm loop (AES-NI / armv8 AES).
        scen("weird/openssl-speed", "alpine:latest")
            .exec("apk add --no-cache openssl >/dev/null 2>&1; openssl speed -seconds 1 -evp aes-128-cbc 2>&1 | grep -q Doing && echo SPEED-OK || echo SPEED-FAIL")
            .has("SPEED-OK").long(),
    ]
}
