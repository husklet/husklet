//! Container OBSERVABILITY commands — `inspect`, `ps`/`ps -a`, `logs`/`logs --tail`/`logs -f`, `top`,
//! `stats` (one-shot), and `port`. One field/format per scenario so a gap maps to a specific query.
//! inspect fields covered: State.Status, Config.Env, Config.Cmd, Mounts, NetworkSettings IP.
//! Host-orchestrated; alpine; ArmLinux. Verified GREEN on the Real docker oracle. Owner: docker-cli
//! agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]).timeout(30) }
/// A darwin-container scenario: the native `macos` image on the ArmMac target (dd backend only — the real
/// docker oracle has no `macos` image, so it self-skips there). The darwin container runs on HOST
/// networking (no AF_UNIX switch), so `-p` needs no daemon forwarder; the daemon-side publish PARSING +
/// REPORTING path is shared with Linux and must work here too, and my forwarder-gate (#320) must let a
/// `-p` darwin container START cleanly instead of colliding with its direct host bind.
fn mac(id: &'static str) -> crate::scenario::Scenario { scen(id, "macos").only(&[Target::ArmMac]).timeout(40) }

pub fn group() -> ScenGroup {
    sgroup("observe", vec![
        // inspect State.Status
        s("observe/inspect-state").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.4
docker inspect -f "{{.State.Status}}" ${C}c"#).has("running"),

        // inspect Config.Env carries -e vars
        s("observe/inspect-config-env").host(r#"
docker run -d --name ${C}c $PLAT -e MARKERENV=zz9 $IMG sleep 60 >/dev/null; sleep 0.3
docker inspect -f "{{range .Config.Env}}{{println .}}{{end}}" ${C}c | grep MARKERENV"#).has("MARKERENV=zz9"),

        // inspect Config.Cmd
        s("observe/inspect-cmd").host(r#"
docker create --name ${C}c $PLAT $IMG echo hicmd >/dev/null
docker inspect -f "{{json .Config.Cmd}}" ${C}c"#).has("echo").has("hicmd"),

        // inspect Mounts destination
        s("observe/inspect-mounts").host(r#"
docker run -d --name ${C}c $PLAT -v "$WORK":/mnt $IMG sleep 60 >/dev/null; sleep 0.3
docker inspect -f "{{range .Mounts}}{{.Destination}}{{end}}" ${C}c"#).has("/mnt"),

        // inspect NetworkSettings has an assigned IP
        s("observe/inspect-network-ip").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker inspect -f "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}" ${C}c | grep -qE "^[0-9]+\." && echo HAS_IP"#).has("HAS_IP"),

        // ps shows a running container with an Up status
        s("observe/ps-running").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker ps --filter name=${C}c --format "{{.Status}}""#).has("Up"),

        // ps -a shows an exited container
        s("observe/ps-all-exited").host(r#"
docker run --name ${C}c $PLAT $IMG true >/dev/null; sleep 0.3
docker ps -a --filter name=${C}c --format "{{.Status}}""#).has("Exited"),

        // ps Ports column shows the published mapping (#320: daemon allocates + reports the host port).
        s("observe/ps-ports").only(&Target::LINUX).host(r#"
docker run -d --name ${C}web $PLAT -p 127.0.0.1::80 $IMG sleep 60 >/dev/null; sleep 0.4
docker ps --filter name=${C}web --format "{{.Ports}}""#).has("->80"),

        // logs captures stdout
        s("observe/logs").host(r#"
docker run --name ${C}c $PLAT $IMG sh -c "echo LOGLINE1; echo LOGLINE2" >/dev/null; sleep 0.3
docker logs ${C}c 2>&1"#).has("LOGLINE1").has("LOGLINE2"),

        // logs --tail N returns only the last N lines (ordered)
        s("observe/logs-tail").host(r#"
docker run --name ${C}c $PLAT $IMG sh -c "for i in 1 2 3 4 5; do echo L\$i; done" >/dev/null; sleep 0.3
docker logs --tail 2 ${C}c 2>&1 | tr "\n" ",""#).has("L4,L5,"),

        // logs -f streams until the container exits
        s("observe/logs-follow").host(r#"
docker run -d --name ${C}c $PLAT $IMG sh -c "echo FOLLOW1; sleep 2; echo FOLLOW2" >/dev/null
docker logs -f ${C}c 2>&1"#).has("FOLLOW1").has("FOLLOW2"),

        // top lists the container's processes
        s("observe/top").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 300 >/dev/null; sleep 0.4
docker top ${C}c 2>&1"#).has("sleep"),

        // stats --no-stream returns a one-shot resource sample
        s("observe/stats-oneshot").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.4
docker stats --no-stream ${C}c >/dev/null 2>&1 && echo STATS_OK"#).has("STATS_OK"),

        // port prints the host mapping for a published container port, honoring the bound host IP
        // (#320: 127.0.0.1 publish reports 127.0.0.1, not 0.0.0.0).
        s("observe/port").only(&Target::LINUX).host(r#"
docker run -d --name ${C}web $PLAT -p 127.0.0.1::80 $IMG sleep 60 >/dev/null; sleep 0.4
docker port ${C}web 80"#).has("127.0.0.1:"),

        // DARWIN container (#320 all-engines): a `-p` publish on the native macos container starts cleanly
        // (host networking → the daemon forwarder is gated OFF for darwin, so it doesn't collide with the
        // container's direct host bind) AND the daemon reports the honored host-IP mapping via `docker
        // port`. dd backend only (the macos image is darwin-native; the real oracle self-skips).
        mac("observe/port-darwin").host(r#"
docker run -d --name ${C}web -p 127.0.0.1:39090:80 $IMG sleep 60 >/dev/null; sleep 0.5
docker inspect -f "{{.State.Running}}" ${C}web
docker port ${C}web 80"#).has("true").has("127.0.0.1:39090"),
    ])
}
