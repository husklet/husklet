//! `docker network` commands + multi-container interaction on a user network. Covers: `network create`
//! + `network ls`, `network rm`, `network connect` (attach a running container to a second network),
//! `network inspect`, and an end-to-end reach: two containers on one user network where the client
//! reaches the server BY NAME (embedded DNS) over TCP. Complements the `netcontainer` group by pinning
//! each network *command*. Host-orchestrated (`$NET` unique + auto-reaped); alpine; ArmLinux. Verified
//! GREEN on the Real docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]).timeout(40) }

pub fn group() -> ScenGroup {
    sgroup("dockernet", vec![
        // create + ls
        s("dockernet/create-ls").host(r#"
docker network create "$NET" >/dev/null
docker network ls --format "{{.Name}}" | grep -q "^${NET}$" && echo NET_LISTED"#).has("NET_LISTED"),

        // rm
        s("dockernet/rm").host(r#"
docker network create "${NET}2" >/dev/null
docker network rm "${NET}2" >/dev/null
docker network ls --format "{{.Name}}" | grep -q "^${NET}2$" && echo STILL || echo NET_REMOVED"#).has("NET_REMOVED"),

        // connect a running container to the network
        s("dockernet/connect").host(r#"
docker network create "$NET" >/dev/null
docker run -d --name ${C}c $PLAT $IMG sleep 120 >/dev/null; sleep 0.3
docker network connect "$NET" ${C}c
docker inspect -f "{{range \$k,\$v := .NetworkSettings.Networks}}{{\$k}} {{end}}" ${C}c | grep -q "$NET" && echo CONNECTED"#).has("CONNECTED"),

        // inspect
        s("dockernet/inspect").host(r#"
docker network create "$NET" >/dev/null
docker network inspect "$NET" >/dev/null 2>&1 && echo NET_INSPECT_OK"#).has("NET_INSPECT_OK"),

        // multi-container: client reaches server BY NAME over TCP on a user network (embedded DNS)
        // GAP (dd, arm): cross-container reach by name fails (both ICMP ping and TCP return nothing) on
        // this build -> embedded-DNS / cross-container routing gap (same class as the netcontainer group).
        // xfail; passes on the Real oracle.
        s("dockernet/reach-by-name").host(r#"
docker network create "$NET" >/dev/null
docker run -d --name ${C}srv --network "$NET" $PLAT $IMG sh -c "while true; do echo BYNAMEOK | nc -l -p 9000 -w 1; done" >/dev/null
sleep 1
docker run --rm --network "$NET" $PLAT $IMG nc -w 3 ${C}srv 9000"#).has("BYNAMEOK").xfail(&[Target::ArmLinux]),
    ])
}
