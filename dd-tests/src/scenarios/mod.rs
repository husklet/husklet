//! The real-software scenario registry. Each category is its OWN folder (`src/scenarios/<cat>/`) owned
//! by one builder agent — agents never edit a shared file, so many run in parallel without collision.
//! This file just declares the folders and aggregates them; add a category = add a folder + two lines.
//!
//! Authoring contract (see docs/CHARTER.md, docs/TESTING.md, docs/IMAGE-MANIFEST.md):
//!   * verify every case against `--backend real` (host docker = ground-truth oracle) so the TEST is
//!     proven correct; then `--backend dd` reveals JIT divergences.
//!   * runs on BOTH linux arches by default; pin output (deterministic); known dd gaps → `.xfail()`.

use crate::scenario::ScenGroup;

pub mod distros;
pub mod databases;
pub mod languages;
pub mod web;
pub mod toolchains;
pub mod utilities;
pub mod weird;
pub mod terminal;
// Core container-behaviour regression net (fast, no heavy installs):
pub mod filesystem;   // rootfs + overlay VFS (no volume)
pub mod permissions;  // file mode/owner semantics + `ls -l` render fidelity (perm strings, maj/min, DAC)
pub mod volumes;      // -v bind mounts (incl. #118 nested `..`)
pub mod networking;   // single-container loopback / DNS / gated outbound
pub mod netinstall;   // apt-get update + install + run (htop) over the network — the field regression net
pub mod netcontainer; // between containers on a user-defined network
pub mod process;      // env / workdir / exit / streams / signals / exec
// Docker-command CONFORMANCE lane (task #310) — one docker CLI flag/verb per scenario so a GA-readiness
// failure is attributable to a specific command. Host-orchestrated, alpine, ArmLinux-scoped (the daemon
// Docker-API path is arch-independent); every case verified GREEN on the Real docker oracle.
pub mod runflags;     // docker run flags: -d/-e/-p/-v/-w/--rm/--name/--entrypoint/--user/--network/--restart/-i/-t/--memory/--cpus
pub mod execcmd;      // docker exec: -e/-w/-u/-d/-i, exit-code, output capture
pub mod lifecycle;    // create/start/stop/kill -s/restart/pause/unpause/wait/rm/rm -f/rename
pub mod observe;      // inspect/ps/ps -a/logs/logs --tail/logs -f/top/stats/port
pub mod cpcmd;        // docker cp host<->container, file + dir
pub mod cpcoherence;  // #374: docker cp into a RUNNING container is visible to the live guest's warm caches
pub mod imagescmd;    // images/tag/rmi/history/image inspect
pub mod buildcmd;     // docker build a small Dockerfile -> image -> run
pub mod dockernet;    // network create/ls/rm/connect/inspect + reach-by-name
pub mod dockervol;    // volume create/ls/rm/inspect + named-volume persistence

pub fn all() -> Vec<ScenGroup> {
    vec![
        distros::group(),
        databases::group(),
        languages::group(),
        web::group(),
        toolchains::group(),
        utilities::group(),
        weird::group(),
        terminal::group(),
        filesystem::group(),
        permissions::group(),
        volumes::group(),
        networking::group(),
        netinstall::group(),
        netcontainer::group(),
        process::group(),
        runflags::group(),
        execcmd::group(),
        lifecycle::group(),
        observe::group(),
        cpcmd::group(),
        cpcoherence::group(),
        imagescmd::group(),
        buildcmd::group(),
        dockernet::group(),
        dockervol::group(),
    ]
}
