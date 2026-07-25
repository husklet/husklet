//! Typed `docker cp` contracts.

use crate::contract::{Group, Scenario, Target};

fn scenario(id: &'static str) -> Scenario {
    Scenario::new(id, "alpine:latest")
        .only(&[Target::Arm64])
        .timeout(30)
}

pub fn group() -> Group {
    Group::new(
        "cpcmd",
        vec![
            scenario("cpcmd/host-to-container-file")
                .host(
                    r#"echo CPFILE > "$WORK/f"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker cp "$WORK/f" ${C}c:/tmp/f
docker exec ${C}c cat /tmp/f"#,
                )
                .contains("CPFILE"),
            scenario("cpcmd/container-to-host-file")
                .host(
                    r#"docker run -d --name ${C}c $PLAT $IMG sh -c "echo FROMCTR > /tmp/g; sleep 60" >/dev/null; sleep 0.5
docker cp ${C}c:/tmp/g "$WORK/g"
cat "$WORK/g""#,
                )
                .contains("FROMCTR"),
            scenario("cpcmd/host-to-container-dir")
                .host(
                    r#"mkdir -p "$WORK/d"; echo AAA > "$WORK/d/a"; echo BBB > "$WORK/d/b"
docker run -d --name ${C}c $PLAT $IMG sleep 60 >/dev/null; sleep 0.3
docker cp "$WORK/d" ${C}c:/tmp/d
docker exec ${C}c sh -c "cat /tmp/d/a; cat /tmp/d/b""#,
                )
                .contains("AAA")
                .contains("BBB"),
            scenario("cpcmd/container-to-host-dir")
                .host(
                    r#"docker run -d --name ${C}c $PLAT $IMG sh -c "mkdir -p /tmp/e; echo XXX>/tmp/e/x; echo YYY>/tmp/e/y; sleep 60" >/dev/null; sleep 0.5
docker cp ${C}c:/tmp/e "$WORK/e"
cat "$WORK/e/x"; cat "$WORK/e/y""#,
                )
                .contains("XXX")
                .contains("YYY"),
        ],
    )
}
