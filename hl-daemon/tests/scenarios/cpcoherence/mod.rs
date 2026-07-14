//! `docker cp` coherence — a daemon-side write into a RUNNING container's filesystem must be
//! visible to the already-running guest promptly (real-Docker/Linux kernel-dcache semantics), even
//! though the engine's path/metadata caches (fscache.c) hold negative/positive entries for the paths.
//! Unlike cpcmd (which reads back via a FRESH `docker exec`, whose new engine process has cold caches),
//! every case here makes the ORIGINAL long-running guest process poll — the process whose warm caches
//! the daemon's write must invalidate (HL_FSGEN_FILE / fsgen_bump). The fix lives in the SHARED syscall
//! dispatch, so it must hold on BOTH engines: each recipe runs on the arch whose alpine the single-arch
//! image store can serve (alpine:latest = arm64, alpine:3.18 = amd64). Verified GREEN on the Real docker
//! oracle. Owner: dockercp-epoch agent.

use crate::scenario::{scen, sgroup, ScenGroup, Scenario, Target};

// cp a NEW file into a live container that is stat-polling for it in its ORIGINAL process: every
// iteration re-caches the ENOENT, so without invalidation the file stays hidden forever. Content-exact
// through the cat.
const NEW_FILE: &str = r#"
echo hello-cp > "$WORK/probe"
docker run -d --name ${C}c $PLAT $IMG sh -c 'i=0; while [ $i -lt 400 ]; do if [ -e /tmp/probe ]; then echo "SEEN:$(cat /tmp/probe)"; exit 0; fi; i=$((i+1)); sleep 0.1; done; echo TIMEOUT; exit 1' >/dev/null
sleep 1
docker cp "$WORK/probe" ${C}c:/tmp/
docker wait ${C}c >/dev/null
docker logs ${C}c"#;

// cp OVER an existing file the guest already stat'd (a warm POSITIVE entry, which is not epoch-gated):
// the guest creates it empty, then polls its SIZE in-process ([ -s ] = stat). A stale positive stat
// would keep reporting size 0 after the cp delivered content.
const OVERWRITE: &str = r#"
echo new-content > "$WORK/probe"
docker run -d --name ${C}c $PLAT $IMG sh -c ': > /tmp/probe; i=0; while [ $i -lt 400 ]; do if [ -s /tmp/probe ]; then echo "GREW:$(cat /tmp/probe)"; exit 0; fi; i=$((i+1)); sleep 0.1; done; echo TIMEOUT; exit 1' >/dev/null
sleep 1
docker cp "$WORK/probe" ${C}c:/tmp/
docker wait ${C}c >/dev/null
docker logs ${C}c"#;

// cp a whole DIRECTORY TREE; the guest polls a DEEP leaf, so the resolver caches negatives for the
// intermediate dirs too (rc_/oc_/updirneg) — all of them must fall to the invalidation.
const DIR_TREE: &str = r#"
mkdir -p "$WORK/d/sub"; echo LEAF-CONTENT > "$WORK/d/sub/leaf"
docker run -d --name ${C}c $PLAT $IMG sh -c 'i=0; while [ $i -lt 400 ]; do if [ -e /tmp/d/sub/leaf ]; then echo "TREE:$(cat /tmp/d/sub/leaf)"; exit 0; fi; i=$((i+1)); sleep 0.1; done; echo TIMEOUT; exit 1' >/dev/null
sleep 1
docker cp "$WORK/d" ${C}c:/tmp/
docker wait ${C}c >/dev/null
docker logs ${C}c"#;

// cp while the guest HOLDS the parent dir open (its cwd pins /tmp/held) and probes with RELATIVE paths;
// after seeing it, list the dir so the merged readdir shows the entry too.
const HELD_OPEN: &str = r#"
echo held-content > "$WORK/probe"
docker run -d --name ${C}c $PLAT $IMG sh -c 'mkdir -p /tmp/held; cd /tmp/held; i=0; while [ $i -lt 400 ]; do if [ -e ./probe ]; then echo "HELD:$(cat ./probe)"; echo "LIST:$(ls)"; exit 0; fi; i=$((i+1)); sleep 0.1; done; echo TIMEOUT; exit 1' >/dev/null
sleep 1
docker cp "$WORK/probe" ${C}c:/tmp/held/
docker wait ${C}c >/dev/null
docker logs ${C}c"#;

fn c(id: &'static str, img: &'static str, tgt: Target, body: &str) -> Scenario {
    scen(id, img).timeout(90).only(&[tgt]).host(body)
}

pub fn group() -> ScenGroup {
    // Each recipe on BOTH engines: arm64 via alpine:latest, amd64 via alpine:3.18 (single-arch store).
    sgroup(
        "cpcoherence",
        vec![
            c(
                "cpcoherence/cp-new-file-live-poll",
                "alpine:latest",
                Target::ArmLinux,
                NEW_FILE,
            )
            .has("SEEN:hello-cp"),
            c(
                "cpcoherence/cp-new-file-live-poll.amd",
                "alpine:3.18",
                Target::AmdLinux,
                NEW_FILE,
            )
            .has("SEEN:hello-cp"),
            c(
                "cpcoherence/cp-overwrite-cached-positive",
                "alpine:latest",
                Target::ArmLinux,
                OVERWRITE,
            )
            .has("GREW:new-content"),
            c(
                "cpcoherence/cp-overwrite-cached-positive.amd",
                "alpine:3.18",
                Target::AmdLinux,
                OVERWRITE,
            )
            .has("GREW:new-content"),
            c(
                "cpcoherence/cp-dir-tree-live-poll",
                "alpine:latest",
                Target::ArmLinux,
                DIR_TREE,
            )
            .has("TREE:LEAF-CONTENT"),
            c(
                "cpcoherence/cp-dir-tree-live-poll.amd",
                "alpine:3.18",
                Target::AmdLinux,
                DIR_TREE,
            )
            .has("TREE:LEAF-CONTENT"),
            c(
                "cpcoherence/cp-into-held-open-dir",
                "alpine:latest",
                Target::ArmLinux,
                HELD_OPEN,
            )
            .has("HELD:held-content")
            .has("LIST:probe"),
            c(
                "cpcoherence/cp-into-held-open-dir.amd",
                "alpine:3.18",
                Target::AmdLinux,
                HELD_OPEN,
            )
            .has("HELD:held-content")
            .has("LIST:probe"),
        ],
    )
}
