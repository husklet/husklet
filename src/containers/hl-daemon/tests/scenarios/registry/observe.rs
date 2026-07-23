//! Typed legacy container-observability contracts.

use crate::contract::{Group, Scenario, Target};

fn host(id: &'static str, command: &str) -> Scenario {
    Scenario::new(id, "alpine:latest")
        .only(&[Target::Arm64])
        .timeout(30)
        .host(command)
}

pub fn group() -> Group {
    Group::new("observe", vec![
        host("observe/inspect-state", "docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.4\ndocker inspect -f \"{{.State.Status}}\" ${C}c").contains("running"),
        host("observe/inspect-config-env", "docker run -d --name ${C}c $PLAT -e MARKERENV=zz9 $IMG sleep 60 >/dev/null; sleep 0.3\ndocker inspect -f \"{{range .Config.Env}}{{println .}}{{end}}\" ${C}c | grep MARKERENV").contains("MARKERENV=zz9"),
        host("observe/inspect-cmd", "docker create --name ${C}c $PLAT $IMG echo hicmd >/dev/null\ndocker inspect -f \"{{json .Config.Cmd}}\" ${C}c").contains("echo").contains("hicmd"),
        host("observe/inspect-mounts", "docker run -d --name ${C}c $PLAT -v \"$WORK\":/mnt $IMG sleep 60 >/dev/null; sleep 0.3\ndocker inspect -f \"{{range .Mounts}}{{.Destination}}{{end}}\" ${C}c").contains("/mnt"),
        host("observe/inspect-network-ip", "docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3\ndocker inspect -f \"{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}\" ${C}c | grep -qE \"^[0-9]+\\.\" && echo HAS_IP").contains("HAS_IP"),
        host("observe/ps-running", "docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3\ndocker ps --filter name=${C}c --format \"{{.Status}}\"").contains("Up"),
        host("observe/ps-all-exited", "docker run --name ${C}c $PLAT $IMG true >/dev/null; sleep 0.3\ndocker ps -a --filter name=${C}c --format \"{{.Status}}\"").contains("Exited"),
        host("observe/ps-ports", "docker run -d --name ${C}web $PLAT -p 127.0.0.1::80 $IMG sleep 60 >/dev/null; sleep 0.4\ndocker ps --filter name=${C}web --format \"{{.Ports}}\"").only(&Target::LINUX).contains("->80"),
        host("observe/logs", "docker run --name ${C}c $PLAT $IMG sh -c \"echo LOGLINE1; echo LOGLINE2\" >/dev/null; sleep 0.3\ndocker logs ${C}c 2>&1").contains("LOGLINE1").contains("LOGLINE2"),
        host("observe/logs-tail", "docker run --name ${C}c $PLAT $IMG sh -c \"for i in 1 2 3 4 5; do echo L\\$i; done\" >/dev/null; sleep 0.3\ndocker logs --tail 2 ${C}c 2>&1 | tr \"\\n\" \",\"").contains("L4,L5,"),
        host("observe/logs-follow", "docker run -d --name ${C}c $PLAT $IMG sh -c \"echo FOLLOW1; sleep 2; echo FOLLOW2\" >/dev/null\ndocker logs -f ${C}c 2>&1").contains("FOLLOW1").contains("FOLLOW2"),
        host("observe/top", "docker run -d --name ${C}c $PLAT $IMG sleep 300 >/dev/null; sleep 0.4\ndocker top ${C}c 2>&1").contains("sleep"),
        host("observe/stats-oneshot", "docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.4\ndocker stats --no-stream ${C}c >/dev/null 2>&1 && echo STATS_OK").contains("STATS_OK"),
        host("observe/port", "docker run -d --name ${C}web $PLAT -p 127.0.0.1::80 $IMG sleep 60 >/dev/null; sleep 0.4\ndocker port ${C}web 80").only(&Target::LINUX).contains("127.0.0.1:"),
        host("observe/container-prune-filter", "docker create --name ${C}keep --label retention=keep $PLAT $IMG true >/dev/null\ndocker create --name ${C}drop --label retention=drop $PLAT $IMG true >/dev/null\ndocker container prune -f --filter label=retention=drop >/dev/null\ndocker inspect ${C}keep >/dev/null && ! docker inspect ${C}drop >/dev/null 2>&1 && echo PRUNE_FILTER_OK").contains("PRUNE_FILTER_OK"),
        host("observe/system-prune-filter-reject", "docker create --name ${C}keep --label retention=keep $PLAT $IMG true >/dev/null\ndocker create --name ${C}drop --label retention=drop $PLAT $IMG true >/dev/null\ndocker system prune -f --filter label!=retention=keep >/dev/null\ndocker inspect ${C}keep >/dev/null && ! docker inspect ${C}drop >/dev/null 2>&1 && echo SYSTEM_FILTER_PRUNE_OK").contains("SYSTEM_FILTER_PRUNE_OK"),
    ])
}
