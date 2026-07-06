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
//! XFAIL POLICY (GAPS.md) — post-#333: NO xfails. The whole cluster now passes on both linux arches on dd.
//! The "exec-loader-noent" gap was a MISDIAGNOSIS: the ELF loader / exec-of-child path was never broken.
//! Two real root causes were found and FIXED:
//!   1. go/rust "not found" / rustup-no-default = the pre-seeded poc images had DROPPED their Config.Env
//!      (PATH=/usr/local/go/bin:/go/bin, /usr/local/cargo/bin + RUSTUP_HOME/CARGO_HOME). Repairing the
//!      image env (a fresh `--long` pull records it automatically) makes go/rust compile+link+run on both
//!      arches — the engine/loader was always correct.
//!   2. gcc/g++/cc + rust-link ICE'd on x86 only ("internal compiler error: in set_static_spec, gcc.cc"):
//!      the non-PIE `.data` pointer-rebasing biased the gcc DRIVER's static_specs pointer table
//!      HIGH while the driver compared it against a LOW-materialized address → gcc_unreachable. Fixed in
//!      translate/x86_64/elf.c by restricting the blind .data rebasing to STATIC non-PIE images (musl jq /
//!      busybox still need it); DYNAMIC non-PIE (glibc gcc/cc1/ld) is low-consistent and must not be rebased.
//! All cases pass on the Real oracle AND on dd (both arches) — hence no `.xfail()` remains.

use crate::scenario::{sgroup, ScenGroup};

mod banners;
mod compile_c;
mod compile_gorust;
mod sanity;

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
pub(super) fn hd(path: &str, body: &str) -> String {
    format!("cat > {path} <<'EOF'\n{body}\nEOF\n")
}

pub fn group() -> ScenGroup {
    sgroup(
        "toolchains",
        compile_c::items()
            .into_iter()
            .chain(compile_gorust::items())
            .chain(banners::items())
            .chain(sanity::items())
            .collect(),
    )
}
