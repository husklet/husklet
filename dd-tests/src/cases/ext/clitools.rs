//! clitools — real CLI-tool coverage via busybox applets in the alpine rootfs.
//! Owner: clitools-coverage agent. Edit ONLY this file. These exercise the container path (rootfs jail,
//! fork/exec of real binaries) end-to-end and are golden-checked. aarch64 (the container rootfs arch).
#![allow(unused_imports)]
use crate::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![clitools()]
}

/// Run `sh -c <cmd>` inside the alpine rootfs.
fn sh(name: &'static str, cmd: &'static str) -> Case {
    in_rootfs(name, "alpine", &["/bin/sh", "-c", cmd])
}

fn clitools() -> Group {
    group("ext-cli", vec![
        // ---- hex / byte dumps ----
        sh("od", "printf ABC | od -An -tx1").out(" 41 42 43\n"),
        sh("hexdump", "printf AB | hexdump -e '/1 \"%02x\"'").out("4142"),
        // ---- digests ----
        sh("sha256", "printf abc | sha256sum")
            .has("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        sh("sha1", "printf abc | sha1sum").has("a9993e364706816aba3e25717850c26c9cd0d89d"),
        sh("cksum", "printf abc | cksum").has("1219131554 3"),
        // ---- base64 decode ----
        sh("base64-d", "printf YWJj | base64 -d").out("abc"),
        // ---- text pipelines ----
        sh("sort-n", "printf '10\\n2\\n1\\n' | sort -n | tr '\\n' ' '").out("1 2 10 "),
        sh("sort-r", "printf 'a\\nb\\nc\\n' | sort -r | tr '\\n' ' '").out("c b a "),
        sh("uniq-c", "printf 'a\\na\\nb\\n' | uniq -c | awk '{print $1$2}' | tr '\\n' ' '").out("2a 1b "),
        sh("wc-c", "printf abcd | wc -c").out("4\n"),
        sh("cut-c", "echo abcdef | cut -c2-4").out("bcd\n"),
        sh("tr-d", "echo a1b2c3 | tr -d 0-9").out("abc\n"),
        sh("head-c", "printf abcdef | head -c 3").out("abc"),
        sh("tail-c", "printf abcdef | tail -c 2").out("ef"),
        sh("grep-c", "printf 'a\\nb\\na\\n' | grep -c a").out("2\n"),
        sh("sed-n", "printf 'a\\nb\\nc\\n' | sed -n 2p").out("b\n"),
        sh("awk-nr", "printf 'a\\nb\\nc\\n' | awk 'END{print NR}'").out("3\n"),
        sh("paste", "printf 'a\\nb\\nc\\n' | paste -sd,").has("a,b,c"),
        sh("fold", "echo abcdef | fold -w2 | tr '\\n' ' '").out("ab cd ef "),
        // ---- archive roundtrip (tar + gzip, fork/exec + fs churn) ----
        sh("tar-gz", "cd /tmp && rm -rf t1 t2 && mkdir t1 t2 && seq 1 50 > t1/f && \
            tar czf - t1 | (cd t2 && tar xzf -) && cmp t1/f t2/t1/f && echo tar-gz-ok; rm -rf t1 t2")
            .out("tar-gz-ok\n"),
    ])
}
