//! `docker build` — build a small local Dockerfile into an image, then run it. Two scenarios: a full
//! FROM/ENV/RUN/COPY/WORKDIR/CMD image (exercises layer build + COPY context + build-time RUN + runtime
//! env/workdir), and a minimal FROM/CMD image. The base is the already-present alpine (FROM resolves
//! from the local store — no outbound pull needed). Built images are `${C}img`-prefixed and removed
//! in-recipe. Host-orchestrated; ArmLinux. Verified GREEN on the Real docker oracle. Owner: docker-cli
//! agent. Edit ONLY this folder.
//!
//! dd implements the CLASSIC Docker Build API (`POST /build` — Dockerfile -> layered image), not the
//! BuildKit gRPC frontend. So each recipe exports `DOCKER_BUILDKIT=0` to force the client onto the
//! classic builder (which POSTs the context tar to `/build`); this is the path dd serves, and Docker
//! Desktop's dockerd serves the identical classic build too, so the Real oracle stays byte-green.
//! Coverage on dd: FROM (local + auto-pull), RUN (via the JIT in a throwaway rootfs), COPY/ADD (context
//! + `--from=<stage>`), ENV, WORKDIR, CMD, ENTRYPOINT, LABEL, ARG/`--build-arg`, multi-stage + `--target`,
//! `--no-cache`, `-t`, and a per-step content-addressed layer cache (see dd-daemon/src/build.rs).

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario {
    scen(id, "alpine:latest")
        .only(&[Target::ArmLinux])
        .timeout(120)
}

pub fn group() -> ScenGroup {
    sgroup("buildcmd", vec![
        // FROM/ENV/RUN/COPY/WORKDIR/CMD -> build -> run, all layers/directives observable at runtime
        s("buildcmd/full").host(r#"
export DOCKER_BUILDKIT=0
printf "FROM alpine:latest\nENV GREETING=BUILT_HELLO\nRUN echo layerdata > /layerfile\nCOPY payload.txt /payload.txt\nWORKDIR /app\nCMD sh -c \"echo \$GREETING; cat /layerfile; cat /payload.txt; pwd\"\n" > "$WORK/Dockerfile"
echo COPIEDPAYLOAD > "$WORK/payload.txt"
docker build $PLAT -t ${C}img:build "$WORK" >/dev/null 2>&1
docker run --rm $PLAT ${C}img:build
docker rmi -f ${C}img:build >/dev/null 2>&1"#).has("BUILT_HELLO").has("layerdata").has("COPIEDPAYLOAD").has("/app"),

        // minimal FROM + CMD
        s("buildcmd/simple").host(r#"
export DOCKER_BUILDKIT=0
printf "FROM alpine:latest\nCMD echo SIMPLEBUILT\n" > "$WORK/Dockerfile"
docker build $PLAT -t ${C}img:s "$WORK" >/dev/null 2>&1
docker run --rm $PLAT ${C}img:s
docker rmi -f ${C}img:s >/dev/null 2>&1"#).has("SIMPLEBUILT"),
    ])
}
