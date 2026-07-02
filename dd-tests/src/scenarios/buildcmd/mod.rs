//! `docker build` — build a small local Dockerfile into an image, then run it. Two scenarios: a full
//! FROM/ENV/RUN/COPY/WORKDIR/CMD image (exercises layer build + COPY context + build-time RUN + runtime
//! env/workdir), and a minimal FROM/CMD image. The base is the already-present alpine (FROM resolves
//! from the local store — no outbound pull needed). Built images are `${C}img`-prefixed and removed
//! in-recipe. Host-orchestrated; ArmLinux. Verified GREEN on the Real docker oracle. Owner: docker-cli
//! agent. Edit ONLY this folder.
//!
//! GAP (dd, arm): `docker build` is NOT implemented by the daemon — it has no /build endpoint, so the
//! CLI falls through to trying to PULL the `-t` tag from the registry ("Pulling from library/…"). Both
//! cases are `.xfail(ArmLinux)` until image build lands; they still PASS on the Real oracle.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]).timeout(120) }

pub fn group() -> ScenGroup {
    sgroup("buildcmd", vec![
        // FROM/ENV/RUN/COPY/WORKDIR/CMD -> build -> run, all layers/directives observable at runtime
        s("buildcmd/full").host(r#"
printf "FROM alpine:latest\nENV GREETING=BUILT_HELLO\nRUN echo layerdata > /layerfile\nCOPY payload.txt /payload.txt\nWORKDIR /app\nCMD sh -c \"echo \$GREETING; cat /layerfile; cat /payload.txt; pwd\"\n" > "$WORK/Dockerfile"
echo COPIEDPAYLOAD > "$WORK/payload.txt"
docker build $PLAT -t ${C}img:build "$WORK" >/dev/null 2>&1
docker run --rm $PLAT ${C}img:build
docker rmi -f ${C}img:build >/dev/null 2>&1"#).has("BUILT_HELLO").has("layerdata").has("COPIEDPAYLOAD").has("/app").xfail(&[Target::ArmLinux]),

        // minimal FROM + CMD
        s("buildcmd/simple").host(r#"
printf "FROM alpine:latest\nCMD echo SIMPLEBUILT\n" > "$WORK/Dockerfile"
docker build $PLAT -t ${C}img:s "$WORK" >/dev/null 2>&1
docker run --rm $PLAT ${C}img:s
docker rmi -f ${C}img:s >/dev/null 2>&1"#).has("SIMPLEBUILT").xfail(&[Target::ArmLinux]),
    ])
}
