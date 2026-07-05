//! Container LIFECYCLE commands — one verb per scenario. Covers: `create`+`start -a`, `stop` (SIGTERM),
//! `kill -s SIGNAL`, `restart`, `pause`/`unpause`, `wait` (blocks then prints exit code), `rm`,
//! `rm -f` (refuses a running container without -f), and `rename`. Host-orchestrated; alpine; ArmLinux.
//! Verified GREEN on the Real docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario {
    scen(id, "alpine:latest")
        .only(&[Target::ArmLinux])
        .timeout(30)
}

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

        // rm <a> <b> <c> : `docker rm` takes MULTIPLE ids/names in one invocation (the CLI issues one
        // DELETE /containers/<ref> per ref). All three stopped containers must be gone after a single rm,
        // and rm must print each removed ref. Parity-verified against the real docker oracle.
        s("lifecycle/rm-multi").host(r#"
docker run --name ${C}a $PLAT $IMG true >/dev/null
docker run --name ${C}b $PLAT $IMG true >/dev/null
docker run --name ${C}c $PLAT $IMG true >/dev/null
OUT=$(docker rm ${C}a ${C}b ${C}c)
echo "printed=$(echo "$OUT" | grep -c "${C}")"
echo "left=$(docker ps -a -q -f name=${C}a -f name=${C}b -f name=${C}c | wc -l | tr -d " ")""#)
            .has("printed=3").has("left=0"),

        // rm -f <a> <b> : force-remove MULTIPLE running containers in one invocation.
        s("lifecycle/rm-multi-force").host(r#"
docker run -d --name ${C}a $PLAT $IMG sleep 300 >/dev/null
docker run -d --name ${C}b $PLAT $IMG sleep 300 >/dev/null; sleep 0.3
docker rm -f ${C}a ${C}b >/dev/null
echo "left=$(docker ps -a -q -f name=${C}a -f name=${C}b | wc -l | tr -d " ")""#).has("left=0"),

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

        // §8.3-3 StopSignal: an image whose Config.StopSignal is SIGQUIT (or --stop-signal) makes a
        // signal-less `docker stop` deliver SIGQUIT, not SIGTERM. The container traps SIGQUIT and exits 0;
        // a TERM-only trap would never fire. Proves stop reads the configured StopSignal.
        s("lifecycle/stop-signal-quit").host(r#"
docker run -d --name ${C}c --stop-signal=SIGQUIT $PLAT $IMG \
  sh -c 'trap "echo GOT_QUIT; exit 0" QUIT; trap "echo GOT_TERM; exit 3" TERM; while true; do sleep 0.2; done' >/dev/null
sleep 0.6
docker stop -t 5 ${C}c >/dev/null
docker logs ${C}c 2>&1
echo "RC=$(docker inspect -f '{{.State.ExitCode}}' ${C}c)""#).has("GOT_QUIT").has("RC=0"),

        // §8.3-3 the configured StopSignal round-trips through inspect Config.StopSignal.
        s("lifecycle/stop-signal-inspect").host(r#"
docker create --name ${C}c --stop-signal=SIGINT $PLAT $IMG sleep 30 >/dev/null
docker inspect -f "{{.Config.StopSignal}}" ${C}c"#).has("SIGINT"),

        // §8.3-2 restart on-failure:N — a container that always fails is restarted EXACTLY N times, then
        // stays stopped. RestartCount settles at N.
        s("lifecycle/restart-on-failure-count").host(r#"
docker run -d --name ${C}c --restart on-failure:2 $PLAT $IMG sh -c "exit 1" >/dev/null
sleep 6
docker inspect -f "count={{.RestartCount}} running={{.State.Running}}" ${C}c"#).has("count=2").has("running=false"),

        // §8.3-5 unless-stopped: a deliberately `docker stop`ped container is NOT resurrected (durable
        // manual-stop). It stays exited after the stop.
        s("lifecycle/unless-stopped-manual").host(r#"
docker run -d --name ${C}c --restart unless-stopped $PLAT $IMG sleep 300 >/dev/null; sleep 0.5
docker stop -t 2 ${C}c >/dev/null; sleep 1.5
docker inspect -f "{{.State.Running}}" ${C}c"#).has("false"),

        // §8.3-1 HEALTHCHECK: a passing `--health-cmd` flips State.Health to healthy; the probe cadence is
        // sub-second so the check resolves quickly.
        s("lifecycle/healthcheck-healthy").host(r#"
docker run -d --name ${C}c $PLAT \
  --health-cmd="true" --health-interval=1s --health-retries=2 --health-timeout=3s \
  $IMG sleep 300 >/dev/null
for i in $(seq 1 15); do
  H=$(docker inspect -f "{{.State.Health.Status}}" ${C}c 2>/dev/null)
  [ "$H" = "healthy" ] && break
  sleep 1
done
echo "HEALTH=$H""#).has("HEALTH=healthy").timeout(40),

        // §8.3-1 HEALTHCHECK: a failing probe flips State.Health to unhealthy after `retries` failures.
        s("lifecycle/healthcheck-unhealthy").host(r#"
docker run -d --name ${C}c $PLAT \
  --health-cmd="false" --health-interval=1s --health-retries=2 --health-timeout=3s \
  $IMG sleep 300 >/dev/null
for i in $(seq 1 20); do
  H=$(docker inspect -f "{{.State.Health.Status}}" ${C}c 2>/dev/null)
  [ "$H" = "unhealthy" ] && break
  sleep 1
done
echo "HEALTH=$H""#).has("HEALTH=unhealthy").timeout(45),
    ])
}
