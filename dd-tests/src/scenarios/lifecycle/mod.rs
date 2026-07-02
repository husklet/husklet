//! Container LIFECYCLE commands — one verb per scenario. Covers: `create`+`start -a`, `stop` (SIGTERM),
//! `kill -s SIGNAL`, `restart`, `pause`/`unpause`, `wait` (blocks then prints exit code), `rm`,
//! `rm -f` (refuses a running container without -f), and `rename`. Host-orchestrated; alpine; ArmLinux.
//! Verified GREEN on the Real docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]).timeout(30) }

pub fn group() -> ScenGroup {
    sgroup("lifecycle", vec![
        // create then start -a : the created container runs and its output attaches
        s("lifecycle/create-start").host(r#"
docker create --name ${C}c $PLAT $IMG echo CREATED_RUN >/dev/null
docker start -a ${C}c"#).has("CREATED_RUN"),

        // stop : container is no longer Running
        s("lifecycle/stop").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 300 >/dev/null; sleep 0.4
docker stop -t 2 ${C}c >/dev/null
docker inspect -f "{{.State.Running}}" ${C}c"#).has("false"),

        // kill -s SIGNAL : the chosen signal is delivered (trap fires)
        s("lifecycle/kill-signal").host(r#"
docker run -d --name ${C}c $PLAT $IMG sh -c "trap \"echo GOT_HUP; exit 0\" HUP; while true; do sleep 0.2; done" >/dev/null
sleep 0.6
docker kill -s HUP ${C}c >/dev/null 2>&1
sleep 0.6
docker logs ${C}c 2>&1"#).has("GOT_HUP"),

        // restart : StartedAt advances and the container is Running again
        s("lifecycle/restart").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 300 >/dev/null; sleep 0.4
S1=$(docker inspect -f "{{.State.StartedAt}}" ${C}c)
docker restart -t 2 ${C}c >/dev/null; sleep 0.4
S2=$(docker inspect -f "{{.State.StartedAt}}" ${C}c)
[ "$S1" != "$S2" ] && docker inspect -f "{{.State.Running}}" ${C}c | grep -q true && echo RESTARTED"#).has("RESTARTED"),

        // pause / unpause : Status transitions paused -> running
        s("lifecycle/pause-unpause").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 300 >/dev/null; sleep 0.4
docker pause ${C}c >/dev/null
P=$(docker inspect -f "{{.State.Status}}" ${C}c)
docker unpause ${C}c >/dev/null
U=$(docker inspect -f "{{.State.Status}}" ${C}c)
echo "P=$P U=$U""#).has("P=paused").has("U=running"),

        // wait : blocks until exit, then prints the exit code
        s("lifecycle/wait").host(r#"
docker run -d --name ${C}c $PLAT $IMG sh -c "sleep 1; exit 17" >/dev/null
docker wait ${C}c"#).has("17"),

        // rm : a stopped container is removed
        s("lifecycle/rm").host(r#"
docker run --name ${C}c $PLAT $IMG true >/dev/null
docker rm ${C}c >/dev/null
docker ps -a -q -f name=${C}c | wc -l | tr -d " ""#).has("0"),

        // rm -f : plain rm refuses a running container; -f force-removes it
        s("lifecycle/rm-force").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 300 >/dev/null; sleep 0.3
docker rm ${C}c 2>&1 | grep -qi "running\|cannot\|force" && echo RM_BLOCKED
docker rm -f ${C}c >/dev/null
echo "LEFT=$(docker ps -a -q -f name=${C}c | wc -l | tr -d " ")""#).has("RM_BLOCKED").has("LEFT=0"),

        // rename : new name resolves, old name is gone
        s("lifecycle/rename").host(r#"
docker run -d --name ${C}old $PLAT $IMG sleep 300 >/dev/null
docker rename ${C}old ${C}new
docker inspect -f "{{.Name}}" ${C}new | grep -q "${C}new" && echo RENAMED
echo "OLDGONE=$(docker ps -a -q -f name=${C}old | wc -l | tr -d " ")""#).has("RENAMED").has("OLDGONE=0"),
    ])
}
