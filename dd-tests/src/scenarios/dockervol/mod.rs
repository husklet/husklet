//! `docker volume` commands + named-volume persistence. Covers: `volume create` + `volume ls`,
//! `volume rm`, `volume inspect`, and the key behaviour — data written into a NAMED volume by one
//! `docker run` is still there for a SECOND independent run (`-v name:/path`). Volumes are `${C}v`-
//! prefixed and removed in-recipe. Host-orchestrated; alpine; ArmLinux. Verified GREEN on the Real
//! docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario { scen(id, "alpine:latest").only(&[Target::ArmLinux]).timeout(30) }

pub fn group() -> ScenGroup {
    sgroup("dockervol", vec![
        // create + ls
        s("dockervol/create-ls").host(r#"
docker volume create ${C}v >/dev/null
docker volume ls --format "{{.Name}}" | grep -q "^${C}v$" && echo VOL_LISTED
docker volume rm ${C}v >/dev/null 2>&1"#).has("VOL_LISTED"),

        // named-volume persistence across two separate runs
        s("dockervol/persist-across-runs").host(r#"
docker volume create ${C}v >/dev/null
docker run --rm $PLAT -v ${C}v:/data $IMG sh -c "echo VOLPERSIST > /data/f"
docker run --rm $PLAT -v ${C}v:/data $IMG cat /data/f
docker volume rm ${C}v >/dev/null 2>&1"#).has("VOLPERSIST"),

        // rm
        s("dockervol/rm").host(r#"
docker volume create ${C}v >/dev/null
docker volume rm ${C}v >/dev/null
docker volume ls --format "{{.Name}}" | grep -q "^${C}v$" && echo STILL || echo VOL_REMOVED"#).has("VOL_REMOVED"),

        // inspect
        s("dockervol/inspect").host(r#"
docker volume create ${C}v >/dev/null
docker volume inspect ${C}v >/dev/null 2>&1 && echo VOL_INSPECT_OK
docker volume rm ${C}v >/dev/null 2>&1"#).has("VOL_INSPECT_OK"),
    ])
}
