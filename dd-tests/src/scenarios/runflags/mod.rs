//! `docker run` FLAGS — the daemon's container-launch contract, one flag per scenario so a failure is
//! attributable to a specific `docker run` option (GA-readiness readout, task #310). Covers: `-d`,
//! `-e`, `-p` (publish + inbound-reachable), `-v` bind mount, `-w`, `--rm`, `--name`, `--entrypoint`,
//! `--user` (uid:gid and name), `--network` (none/bridge), `--restart`, exit-code propagation, `-i`
//! stdin, `-t` PTY, and `--memory`/`--cpus` (accepted + cgroup-honored). Host-orchestrated (a real
//! docker command is the thing under test); alpine only, scoped to ArmLinux — the docker-command layer
//! is arch-independent (same daemon code), so one guest arch exercises the API handler unambiguously.
//! Every case verified GREEN on the Real docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]) }

pub fn group() -> ScenGroup {
    sgroup("runflags", vec![
        // -d : detached; container is Running after launch
        s("runflags/detached-d").host(r#"
docker run -d --name ${C}c $PLAT $IMG sleep 30 >/dev/null
sleep 0.5
docker inspect -f "{{.State.Running}}" ${C}c"#).has("true").timeout(30),

        // -e : env-var passthrough
        s("runflags/env-e").host(r#"
docker run --rm $PLAT -e FOO=barbaz $IMG printenv FOO"#).has("barbaz"),

        // -p : publish a container port to a host port; the published port is reachable from the host
        // PARTIAL (dd, arm): the daemon now ALLOCATES the host port for the `-p 127.0.0.1::9000` auto-assign
        // form and reports it via `docker port`/`ps`/inspect (so observe/ps-ports/port pass), and the engine's
        // host forwarder reaches a stable long-lived listener. This recipe re-listens every second
        // (`nc -l -w 1` loop); the forwarder is bound to the guest process that called listen(), so it churns
        // on each re-listen and the single host connect races that window -> still xfail. A normal server that
        // binds once (nginx/redis) is reachable. Fixing the re-listen case needs a process-independent
        // forwarder (engine architecture).
        s("runflags/publish-p").host(r#"
docker run -d --name ${C}svc $PLAT -p 127.0.0.1::9000 $IMG sh -c "while true; do echo PUBOK | nc -l -p 9000 -w 1; done" >/dev/null
sleep 1
HP=$(docker port ${C}svc 9000/tcp | head -1 | sed "s/.*://")
nc -w 2 127.0.0.1 $HP"#).has("PUBOK").timeout(30).xfail(&[Target::ArmLinux]),

        // -v : bind mount, data readable + writable both ways
        s("runflags/bind-mount-v").host(r#"
echo SEED > "$WORK/f"
docker run --rm $PLAT -v "$WORK":/m $IMG sh -c "cat /m/f; echo WROTE > /m/g"
cat "$WORK/g""#).has("SEED").has("WROTE"),

        // -w : working directory
        s("runflags/workdir-w").host(r#"
docker run --rm $PLAT -w /var/spool $IMG pwd"#).has("/var/spool"),

        // --rm : container is auto-removed after it exits
        s("runflags/rm").host(r#"
docker run --rm --name ${C}c $PLAT $IMG true
sleep 0.3
docker ps -a -q -f name=${C}c | wc -l | tr -d " ""#).has("0"),

        // --name : the assigned name is stored + queryable
        s("runflags/name").host(r#"
docker run -d --name ${C}named $PLAT $IMG sleep 30 >/dev/null
docker inspect -f "{{.Name}}" ${C}named | grep -q "${C}named" && echo NAME_OK"#).has("NAME_OK").timeout(30),

        // --entrypoint : override the image entrypoint
        s("runflags/entrypoint").host(r#"
docker run --rm --entrypoint echo $PLAT $IMG ENTRYOVERRIDE"#).has("ENTRYOVERRIDE"),

        // --user uid:gid
        s("runflags/user-uidgid").host(r#"
docker run --rm --user 1000:1000 $PLAT $IMG id"#).has("uid=1000").has("gid=1000"),

        // --user by name (nobody -> 65534)
        s("runflags/user-name").host(r#"
docker run --rm --user nobody $PLAT $IMG id -u"#).has("65534"),

        // --network none : no eth0, container is isolated (loopback-only)
        s("runflags/network-none").host(r#"
docker run --rm --network none $PLAT $IMG sh -c "ip -o link 2>/dev/null | grep -q eth0 && echo HAS_ETH || echo NO_ETH""#).has("NO_ETH"),

        // default bridge network : eth0 present
        s("runflags/network-bridge").host(r#"
docker run --rm $PLAT $IMG sh -c "ip -o link show eth0 >/dev/null 2>&1 && echo HAS_ETH0""#).has("HAS_ETH0"),

        // --restart on-failure:N : a failing container is restarted up to N times
        s("runflags/restart-on-failure").host(r#"
docker run -d --name ${C}r --restart on-failure:3 $PLAT $IMG sh -c "exit 1" >/dev/null
rc=0
for i in $(seq 1 40); do rc=$(docker inspect -f "{{.RestartCount}}" ${C}r 2>/dev/null); [ "$rc" = "3" ] && break; sleep 0.3; done
echo "RESTARTS=$rc""#).has("RESTARTS=3").timeout(40),

        // exit-code propagation into State.ExitCode
        s("runflags/exit-code").host(r#"
docker run --name ${C}e $PLAT $IMG sh -c "exit 42" >/dev/null 2>&1
docker inspect -f "{{.State.ExitCode}}" ${C}e"#).has("42"),

        // -i : stdin piped into the container
        s("runflags/stdin-i").host(r#"
echo HELLOSTDIN | docker run -i --rm $PLAT $IMG cat"#).has("HELLOSTDIN"),

        // -t : allocate a PTY -> isatty(1) is true
        s("runflags/tty-t").host(r#"
docker run --rm -t $PLAT $IMG sh -c "test -t 1 && echo IS_TTY || echo NO_TTY" | tr -d "\r""#).has("IS_TTY"),

        // --memory : flag accepted, container runs
        s("runflags/memory-accepted").host(r#"
docker run --rm --memory 64m $PLAT $IMG echo MEMFLAG_OK"#).has("MEMFLAG_OK"),

        // --memory : limit is actually reflected in the cgroup (64m = 67108864)
        s("runflags/memory-cgroup-honored").host(r#"
docker run --rm --memory 64m $PLAT $IMG sh -c "cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes 2>/dev/null""#).has("67108864"),

        // --cpus : flag accepted, container runs
        s("runflags/cpus-accepted").host(r#"
docker run --rm --cpus 0.5 $PLAT $IMG echo CPUFLAG_OK"#).has("CPUFLAG_OK"),
    ])
}
