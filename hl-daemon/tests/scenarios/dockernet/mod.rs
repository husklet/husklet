//! `docker network` commands + multi-container interaction on a user network. Covers: `network create`
//! + `network ls`, `network rm`, `network connect` (attach a running container to a second network),
//! `network inspect`, and an end-to-end reach: two containers on one user network where the client
//! reaches the server BY NAME (embedded DNS) over TCP. Complements the `netcontainer` group by pinning
//! each network *command*. Host-orchestrated (`$NET` unique + auto-reaped); alpine; ArmLinux. Verified
//! GREEN on the Real docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario {
    scen(id, "alpine:latest")
        .only(&[Target::ArmLinux])
        .timeout(40)
}

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

        // multi-container: client reaches server BY NAME over TCP on a user network (embedded DNS).
        // Server launches BEFORE the client, so the client's launch-time /etc/hosts snapshot already
        // carries the peer — the baseline reach-by-name path. Both Linux arches.
        s("dockernet/reach-by-name").only(&Target::LINUX).host(r#"
docker network create "$NET" >/dev/null
docker run -d --name ${C}srv --network "$NET" $PLAT $IMG sh -c "while true; do echo BYNAMEOK | nc -l -p 9000 -w 1; done" >/dev/null
sleep 1
docker run --rm --network "$NET" $PLAT $IMG nc -w 3 ${C}srv 9000"#).has("BYNAMEOK"),

        // the REAL reach-by-name gap: a peer that appears AFTER the resolving container launched. The
        // client idles first (its /etc/hosts is frozen at launch, WITHOUT the server); the server joins the
        // network only afterwards; then the client resolves it BY NAME via `docker exec`. A static
        // /etc/hosts snapshot can't see the late peer — only the live in-engine 127.0.0.11 resolver
        // (consulting the daemon's live per-network names file) can. Real docker's embedded DNS passes;
        // hl passes once the resolver reads live daemon state.
        s("dockernet/reach-by-name-late").only(&Target::LINUX).host(r#"
docker network create "$NET" >/dev/null
docker run -d --name ${C}cli --network "$NET" $PLAT $IMG sleep 120 >/dev/null
sleep 0.5
docker run -d --name ${C}srv --network "$NET" $PLAT $IMG sh -c "while true; do echo LATEOK | nc -l -p 9000 -w 1; done" >/dev/null
sleep 1.5
docker exec ${C}cli nc -w 3 ${C}srv 9000"#).has("LATEOK").timeout(40),
    ])
}
