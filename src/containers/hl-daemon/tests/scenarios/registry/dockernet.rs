//! Typed legacy Docker network-command contracts.

use crate::contract::{Group, Scenario, Target};

fn host(id: &'static str, command: &str) -> Scenario {
    Scenario::new(id, "alpine:latest")
        .only(&[Target::Arm64])
        .timeout(40)
        .host(command)
}

pub fn group() -> Group {
    Group::new("dockernet", vec![
        host("dockernet/create-ls", r#"docker network create "$NET" >/dev/null
docker network ls --format "{{.Name}}" | grep -q "^${NET}$" && echo NET_LISTED"#).contains("NET_LISTED"),
        host("dockernet/rm", r#"docker network create "${NET}2" >/dev/null
docker network rm "${NET}2" >/dev/null
docker network ls --format "{{.Name}}" | grep -q "^${NET}2$" && echo STILL || echo NET_REMOVED"#).contains("NET_REMOVED"),
        host("dockernet/connect", r#"docker network create "$NET" >/dev/null
docker run -d --name ${C}c $PLAT $IMG sleep 120 >/dev/null; sleep 0.3
docker network connect "$NET" ${C}c
docker inspect -f "{{range \$k,\$v := .NetworkSettings.Networks}}{{\$k}} {{end}}" ${C}c | grep -q "$NET" && echo CONNECTED"#).contains("CONNECTED"),
        host("dockernet/inspect", r#"docker network create "$NET" >/dev/null
docker network inspect "$NET" >/dev/null 2>&1 && echo NET_INSPECT_OK"#).contains("NET_INSPECT_OK"),
        host("dockernet/reach-by-name", r#"docker network create "$NET" >/dev/null
docker run -d --name ${C}srv --network "$NET" $PLAT $IMG sh -c "while true; do echo BYNAMEOK | nc -l -p 9000 -w 1; done" >/dev/null
sleep 1
docker run --rm --network "$NET" $PLAT $IMG nc -w 3 ${C}srv 9000"#).only(&Target::LINUX).contains("BYNAMEOK"),
        host("dockernet/reach-by-name-late", r#"docker network create "$NET" >/dev/null
docker run -d --name ${C}cli --network "$NET" $PLAT $IMG sleep 120 >/dev/null
sleep 0.5
docker run -d --name ${C}srv --network "$NET" $PLAT $IMG sh -c "while true; do echo LATEOK | nc -l -p 9000 -w 1; done" >/dev/null
sleep 1.5
docker exec ${C}cli nc -w 3 ${C}srv 9000"#).only(&Target::LINUX).contains("LATEOK").timeout(40),
        host("dockernet/create-multi-alias", r#"docker network create "$NET" >/dev/null
docker network create "${NET}2" >/dev/null
docker create --name ${C}srv --network "$NET" --network-alias front --network "${NET}2" --network-alias database $PLAT $IMG sh -c "echo MULTIOK | nc -l -p 9000" >/dev/null
docker create --name ${C}cli --network "$NET" --network "${NET}2" $PLAT $IMG sleep 120 >/dev/null
docker start ${C}srv ${C}cli >/dev/null
docker exec ${C}cli nc -w 3 database 9000"#).only(&Target::LINUX).contains("MULTIOK"),
        host("dockernet/host-mode", r"docker run --rm --network host $PLAT $IMG sh -c 'test -r /etc/resolv.conf && test -s /etc/hosts && echo HOST_MODE_OK'").contains("HOST_MODE_OK"),
    ])
}
