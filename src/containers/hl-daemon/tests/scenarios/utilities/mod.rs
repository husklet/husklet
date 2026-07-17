//! Utilities / devtools — the everyday developer-at-a-shell surface. busybox, coreutils, sed/awk/grep,
//! tar/gzip, jq, openssl, git, curl, socat, bash. Deterministic, HERMETIC (no network: loopback only,
//! no package installs) workflows that push fork-heavy pipelines, text/crypto codegen, and real tool
//! binaries through the JIT. Both Linux arches. Owner: utilities agent. Recipes: docs/IMAGE-MANIFEST §6.
//!
//! Harness constraint discovered during authoring: single-tool images (jq, alpine/git, alpine/openssl,
//! alpine/socat, curlimages/curl) use the TOOL as their ENTRYPOINT, so the `exec` form (which appends
//! `/bin/sh -c …`) can't drive them — those use `.run(argv)`. Shell workflows use base images
//! (alpine/busybox/debian) or `bitnami/git` (passthrough entrypoint + bash). `bash:5.2` ships bash only
//! at /usr/local/bin (no /bin/bash) → it also uses `.run(&["bash","-c",…])`.
//!
//! All hashes are published vectors or verified-once-and-pinned on the Real oracle (Docker Desktop):
//!   sha256("abc")=ba7816bf… · sha256("")=e3b0c442… · md5("abc")=90015098… · empty git blob=e69de29b…
//!   git blob "hl\n"=f03f6945… · fixed-identity commit SHA=9fba1c3d… (all inputs pinned → reproducible).
//!
//! One file per tool/concern category (each returns `Vec<Scenario>`); `group()` chains them in order.

use crate::scenario::{sgroup, ScenGroup};

mod archives;
mod arithmetic;
mod bash;
mod busybox;
mod crypto;
mod fork;
mod git;
mod jq;
mod net;
mod overlay;
mod staticexec;
mod text;

pub fn group() -> ScenGroup {
    sgroup(
        "utilities",
        crypto::items()
            .into_iter()
            .chain(text::items())
            .chain(arithmetic::items())
            .chain(fork::items())
            .chain(archives::items())
            .chain(overlay::items())
            .chain(busybox::items())
            .chain(bash::items())
            .chain(jq::items())
            .chain(git::items())
            .chain(net::items())
            .chain(staticexec::items())
            .collect(),
    )
}
