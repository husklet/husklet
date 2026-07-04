//! scratchx — #231 scratch / distroless image loader-exec guard. Owner: scratch-exec agent. Edit ONLY
//! this file. Keep it compiling (`cargo build -p dd-tests`).
//!
//! A FROM-scratch / distroless / Go-microservice image is just a single static binary in an otherwise
//! EMPTY rootfs — no shell, no interpreter (a static binary has no PT_INTERP), no libc on disk, nothing
//! but the program (nats-server, hello-world's `/hello`, gcr.io/distroless/static:*). The loader/exec
//! path must resolve argv[0] INSIDE that scratch jail (xresolve_overlay) and enter the guest without
//! opening any absent helper path (an interpreter, a shell, a re-opened exe). A regression that made the
//! loader reach for a missing path would resurface as "ELF loader/exec failed: open: No such file".
//!
//! `.scratch()` runs the compiled static-PIE guest as the SOLE executable in a synthesized empty rootfs,
//! on BOTH Linux engines. Fixed golden output ⇒ the check asserts the loader entered the guest, byte for
//! byte, exactly as the native oracle would. (Verified end-to-end: the real nats:latest static Go binary
//! runs `nats-server --version` byte-exact vs the OrbStack docker oracle under this same loader path.)

use crate::{group, src, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![group("scratchx", vec![
        src("scratch-static-exec", "scratch_exec.c")
            .scratch()
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("scratch-exec OK\n"),
    ])]
}
