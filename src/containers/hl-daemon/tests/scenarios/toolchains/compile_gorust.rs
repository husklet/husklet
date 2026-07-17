//! Go + Rust compile-and-run. `go build/run` (codegen + internal linker) and `rustc` (rustc/LLVM
//! codegen; the LINK step shells out to `cc`/gcc). Both work on BOTH arches once the image PATH is
//! present and the x86 gcc-driver ICE is fixed. Both arches, no xfail.

use super::{hd, GO_FIB, GO_SUM, RS_FIB, RS_SUM};
use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();

    // -- go build/run (codegen + internal linker) — works on BOTH arches once the image PATH is present.
    let go_env = "export GOCACHE=/tmp/gocache GOFLAGS=-mod=mod\n";
    v.push(
        scen("toolchains/go-123-run-sum", "golang:1.23")
            .exec(&format!("{}{}go run /m.go", go_env, hd("/m.go", GO_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/go-122-alpine-fib", "golang:1.22-alpine")
            .exec(&format!("{}{}go run /m.go", go_env, hd("/m.go", GO_FIB)))
            .has("R=12586269025")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/go-122-bookworm-build", "golang:1.22-bookworm")
            .exec(&format!(
                "{}{}go build -o /m /m.go && /m",
                go_env,
                hd("/m.go", GO_SUM)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/go-121-alpine-sum", "golang:1.21-alpine")
            .exec(&format!("{}{}go run /m.go", go_env, hd("/m.go", GO_SUM)))
            .has("R=500500")
            .timeout(180)
            .long(),
    );

    // -- rustc — rustc/LLVM codegen works on both arches, but the LINK step shells out to `cc` (gcc);
    // the LINK step shells out to `cc` (gcc); the x86 gcc-driver ICE is fixed, so rust runs on BOTH arches.
    v.push(
        scen("toolchains/rust-179-slim-sum", "rust:1.79-slim")
            .exec(&format!(
                "{}rustc -O /m.rs -o /m && /m",
                hd("/m.rs", RS_SUM)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/rust-179-sum", "rust:1.79")
            .exec(&format!(
                "{}rustc -O /m.rs -o /m && /m",
                hd("/m.rs", RS_SUM)
            ))
            .has("R=500500")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/rust-178-slim-fib", "rust:1.78-slim")
            .exec(&format!(
                "{}rustc -O /m.rs -o /m && /m",
                hd("/m.rs", RS_FIB)
            ))
            .has("R=12586269025")
            .timeout(180)
            .long(),
    );
    v.push(
        scen("toolchains/rust-178-alpine-fib", "rust:1.78-alpine")
            .exec(&format!(
                "{}rustc -O /m.rs -o /m && /m",
                hd("/m.rs", RS_FIB)
            ))
            .has("R=12586269025")
            .timeout(180)
            .long(),
    );

    v
}
