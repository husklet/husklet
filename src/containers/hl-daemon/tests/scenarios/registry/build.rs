//! Typed legacy image-build contracts.

use crate::contract::{Group, Scenario, Target};

fn scenario(id: &'static str) -> Scenario {
    Scenario::new(id, "alpine:latest")
        .only(&[Target::Arm64])
        .timeout(120)
}

pub fn group() -> Group {
    Group::new(
        "buildcmd",
        vec![
            scenario("buildcmd/full")
                .host(
                    r#"export DOCKER_BUILDKIT=0
printf "FROM alpine:latest\nENV GREETING=BUILT_HELLO\nRUN echo layerdata > /layerfile\nCOPY payload.txt /payload.txt\nWORKDIR /app\nCMD sh -c \"echo \$GREETING; cat /layerfile; cat /payload.txt; pwd\"\n" > "$WORK/Dockerfile"
echo COPIEDPAYLOAD > "$WORK/payload.txt"
docker build $PLAT -t ${C}img:build "$WORK" >/dev/null 2>&1
docker run --rm $PLAT ${C}img:build
docker rmi -f ${C}img:build >/dev/null 2>&1"#,
                )
                .contains("BUILT_HELLO")
                .contains("layerdata")
                .contains("COPIEDPAYLOAD")
                .contains("/app"),
            scenario("buildcmd/simple")
                .host(
                    r#"export DOCKER_BUILDKIT=0
printf "FROM alpine:latest\nCMD echo SIMPLEBUILT\n" > "$WORK/Dockerfile"
docker build $PLAT -t ${C}img:s "$WORK" >/dev/null 2>&1
docker run --rm $PLAT ${C}img:s
docker rmi -f ${C}img:s >/dev/null 2>&1"#,
                )
                .contains("SIMPLEBUILT"),
        ],
    )
}
