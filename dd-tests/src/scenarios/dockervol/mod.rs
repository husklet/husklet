//! `docker volume` commands + named-volume persistence. Covers: `volume create` + `volume ls`,
//! `volume rm`, `volume inspect`, and the key behaviour — data written into a NAMED volume by one
//! `docker run` is still there for a SECOND independent run (`-v name:/path`). Volumes are `${C}v`-
//! prefixed and removed in-recipe. Host-orchestrated; alpine; ArmLinux. Verified GREEN on the Real
//! docker oracle. Owner: docker-cli agent. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup, Target};

fn s(id: &'static str) -> crate::scenario::Scenario {
    scen(id, "alpine:latest")
        .only(&[Target::ArmLinux])
        .timeout(30)
}

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

        // §6.3-1 tmpfs: `--tmpfs /x` is a FRESH, writable, empty mount each run. Write a file, see it in
        // the same run; a SECOND run of the same image sees an empty /x (tmpfs is never persisted).
        s("dockervol/tmpfs-fresh").host(r#"
docker run --rm $PLAT --tmpfs /cache $IMG sh -c "echo hi > /cache/f && cat /cache/f && ls -1 /cache | wc -l | tr -d ' '"
docker run --rm $PLAT --tmpfs /cache $IMG sh -c "ls -1 /cache | wc -l | tr -d ' '""#).has("hi").has("1").has("0"),

        // §6.3-1 tmpfs via `--mount type=tmpfs`: same fresh-empty semantics, and it shows up in inspect.
        s("dockervol/mount-tmpfs").host(r#"
docker run -d --name ${C}c $PLAT --mount type=tmpfs,destination=/scratch $IMG sleep 60 >/dev/null; sleep 0.4
docker exec ${C}c sh -c "echo ok > /scratch/f && cat /scratch/f"
docker inspect -f "{{range .Mounts}}{{.Type}} {{.Destination}}{{end}}" ${C}c"#).has("ok").has("tmpfs /scratch"),

        // §6.3-3 bare `-v /path` anonymous volume: docker creates an anon volume and mounts it there;
        // its data persists across runs of the SAME container (start/stop), and inspect shows Type=volume.
        s("dockervol/anon-volume").host(r#"
docker run -d --name ${C}c $PLAT -v /data $IMG sh -c "echo ANON > /data/f; sleep 60" >/dev/null; sleep 0.6
docker exec ${C}c cat /data/f
docker inspect -f "{{range .Mounts}}{{.Type}}:{{.Destination}} {{end}}" ${C}c | grep -q "volume:/data" && echo ANON_IS_VOLUME"#).has("ANON").has("ANON_IS_VOLUME"),

        // §6.3-6 a `--mount type=volume` volume is IN USE — `volume rm` refuses it while the container
        // lives (previously only `-v`/Binds were scanned, so a --mount volume looked unused).
        s("dockervol/mount-volume-inuse").host(r#"
docker volume create ${C}v >/dev/null
docker run -d --name ${C}c $PLAT --mount type=volume,source=${C}v,destination=/data $IMG sleep 60 >/dev/null; sleep 0.5
docker volume rm ${C}v 2>&1 | grep -qi "in use\|cannot" && echo IN_USE
docker rm -f ${C}c >/dev/null; docker volume rm ${C}v >/dev/null 2>&1"#).has("IN_USE"),
    ])
}
