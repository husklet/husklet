//! `docker exec` into a RUNNING container — one option per scenario so a failure maps to a specific
//! exec flag. Covers: plain exec (output captured), `-e` env, `-w` workdir, `-u` user, `-d` detached,
//! exit-code propagation, and `-i` stdin. Each recipe boots its own idle `${C}c` container then execs
//! into it. Host-orchestrated; alpine; ArmLinux (arch-independent daemon path). Verified GREEN on the
//! Real docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario {
    scen(id, "alpine:latest")
        .only(&[Target::ArmLinux])
        .timeout(30)
}

pub fn group() -> ScenGroup {
    sgroup(
        "execcmd",
        vec![
            // plain exec, stdout captured
            s("execcmd/basic")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker exec ${C}c echo EXECOK"#,
                )
                .has("EXECOK"),
            // exec -e ENV
            s("execcmd/env-e")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker exec -e XX=yyval ${C}c printenv XX"#,
                )
                .has("yyval"),
            // exec -w WORKDIR
            s("execcmd/workdir-w")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker exec -w /etc ${C}c pwd"#,
                )
                .has("/etc"),
            // exec -u USER
            s("execcmd/user-u")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker exec -u 1000 ${C}c id -u"#,
                )
                .has("1000"),
            // exec -d detached; side effect is visible on a later exec
            s("execcmd/detached-d")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker exec -d ${C}c sh -c "echo DETACHEDWROTE > /tmp/d"; sleep 0.5
docker exec ${C}c cat /tmp/d"#,
                )
                .has("DETACHEDWROTE"),
            // exec exit-code propagation to the client
            s("execcmd/exit-code")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker exec ${C}c sh -c "exit 9"; echo RC=$?"#,
                )
                .has("RC=9"),
            // exec -i stdin piped in
            s("execcmd/stdin-i")
                .host(
                    r#"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
echo INPUTLINE | docker exec -i ${C}c cat"#,
                )
                .has("INPUTLINE"),
        ],
    )
}
