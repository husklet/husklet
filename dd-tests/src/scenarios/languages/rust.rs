//! Rust — rustc drives LLVM codegen then links and runs the produced binary: a real native-codegen
//! pipeline (cc1-class compile + ld + exec) inside the container. musl (alpine) + glibc (slim).
//! exec-loader-noent (#333) triage: NOT an exec-loader gap. rustc runs and compiles on both arches once
//! the image Config.Env (PATH=/usr/local/cargo/bin, RUSTUP_HOME, CARGO_HOME) is present. On ARM the whole
//! compile+link+run passes; on x86 the LINK step shells out to `cc` (gcc), whose driver ICEs in
//! set_static_spec under the x86_64 JIT (the one real remaining engine bug) → COMPILE cases xfail AmdLinux
//! only. `rustc --version` does not link → passes both arches. Floating 1.x tags for stable availability.

use crate::scenario::{scen, Scenario, Target};

pub fn scenarios() -> Vec<Scenario> {
    vec![
        scen("languages/rust-sum-1-alpine", "rust:1-alpine")
            .exec("cat > /m.rs <<'EOF'\nfn main(){ let s:u64=(1..=1000).sum(); println!(\"{}\",s); }\nEOF\nrustc /m.rs -o /m && /m")
            .has("500500")
            .long()
            .xfail(&[Target::AmdLinux]), // x86 gcc-driver link ICE (set_static_spec)
        scen("languages/rust-fib-1-slim", "rust:1-slim")
            .exec("cat > /m.rs <<'EOF'\nfn main(){ let (mut a,mut b):(u64,u64)=(0,1); for _ in 0..50 {let t=a+b;a=b;b=t;} println!(\"{}\",a); }\nEOF\nrustc /m.rs -o /m && /m")
            .has("12586269025")
            .long()
            .xfail(&[Target::AmdLinux]),
        scen("languages/rust-version-1-slim", "rust:1-slim")
            .exec("rustc --version | grep -o 'rustc 1.'")
            .has("rustc 1."),
    ]
}
