//! Rust — rustc drives LLVM codegen then links and runs the produced binary: a real native-codegen
//! pipeline (cc1-class compile + ld + exec) inside the container. musl (alpine) + glibc (slim).
//! exec-loader-noent triage — NOT an exec-loader gap, and now NO xfail: rustc compiles, links and
//! runs on BOTH arches under hl. Two fixes made it pass: (1) the poc image must carry Config.Env
//! (PATH=/usr/local/cargo/bin, RUSTUP_HOME, CARGO_HOME) — a fresh `--long` pull records it; (2) on x86 the
//! LINK step shells out to `cc` (gcc), whose driver ICE'd in set_static_spec until the non-PIE `.data`
//! rebasing was restricted to static images (translate/x86_64/elf.c). Floating 1.x tags for availability.

use crate::scenario::{scen, Scenario};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        scen("languages/rust-sum-1-alpine", "rust:1-alpine")
            .exec("cat > /m.rs <<'EOF'\nfn main(){ let s:u64=(1..=1000).sum(); println!(\"{}\",s); }\nEOF\nrustc /m.rs -o /m && /m")
            .has("500500")
            .long(),
        scen("languages/rust-fib-1-slim", "rust:1-slim")
            .exec("cat > /m.rs <<'EOF'\nfn main(){ let (mut a,mut b):(u64,u64)=(0,1); for _ in 0..50 {let t=a+b;a=b;b=t;} println!(\"{}\",a); }\nEOF\nrustc /m.rs -o /m && /m")
            .has("12586269025")
            .long(),
        scen("languages/rust-version-1-slim", "rust:1-slim")
            .exec("rustc --version | grep -o 'rustc 1.'")
            .has("rustc 1."),
    ]
}
