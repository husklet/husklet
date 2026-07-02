//! IMAGE commands — `images` (list), `tag`, `rmi`, `history`, and `image inspect`. One verb per
//! scenario; images created by a test are `${C}img`-prefixed and removed in-recipe. NOTE: `docker pull`
//! of a fresh image is NOT covered here — this host's Little Snitch blocks the engine's outbound
//! registry TCP (task #310 caveat), so a live pull would hang; "image already present" is asserted via
//! `image inspect`/`images` instead. alpine base; ArmLinux. Verified GREEN on the Real docker oracle.
//! Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]).timeout(30) }

pub fn group() -> ScenGroup {
    sgroup("imagescmd", vec![
        // images : the base image is listed
        s("imagescmd/list").host(r#"
docker images --format "{{.Repository}}:{{.Tag}}" | grep -q "^alpine:latest" && echo ALPINE_LISTED"#).has("ALPINE_LISTED"),

        // tag : a new tag appears in the image list
        s("imagescmd/tag").host(r#"
docker tag alpine:latest ${C}img:v1
docker images --format "{{.Repository}}:{{.Tag}}" | grep -q "${C}img:v1" && echo TAGGED
docker rmi ${C}img:v1 >/dev/null 2>&1"#).has("TAGGED"),

        // rmi : a tag is removed
        s("imagescmd/rmi").host(r#"
docker tag alpine:latest ${C}img:rmv
docker rmi ${C}img:rmv >/dev/null
docker images --format "{{.Repository}}:{{.Tag}}" | grep -q "${C}img:rmv" && echo STILL_THERE || echo RMI_OK"#).has("RMI_OK"),

        // history : layer history is returned
        s("imagescmd/history").host(r#"
docker history --no-trunc alpine:latest >/dev/null 2>&1 && echo HISTORY_OK"#).has("HISTORY_OK"),

        // image inspect : os/arch metadata
        s("imagescmd/inspect").host(r#"
docker image inspect -f "{{.Os}}/{{.Architecture}}" alpine:latest"#).has("linux/"),
    ])
}
