//! Typed legacy managed-volume contracts.

use crate::contract::{Group, Scenario, Target};

fn host(id: &'static str, command: &str) -> Scenario {
    Scenario::new(id, "alpine:latest")
        .only(&[Target::Arm64])
        .timeout(30)
        .host(command)
}

pub fn group() -> Group {
    Group::new(
        "dockervol",
        vec![
            host("dockervol/create-ls", r#"docker volume create ${C}v >/dev/null
docker volume ls --format "{{.Name}}" | grep -q "^${C}v$" && echo VOL_LISTED
docker volume rm ${C}v >/dev/null 2>&1"#).contains("VOL_LISTED"),
            host("dockervol/persist-across-runs", r#"docker volume create ${C}v >/dev/null
docker run --rm $PLAT -v ${C}v:/data $IMG sh -c "echo VOLPERSIST > /data/f"
docker run --rm $PLAT -v ${C}v:/data $IMG cat /data/f
docker volume rm ${C}v >/dev/null 2>&1"#).contains("VOLPERSIST"),
            host("dockervol/rm", r#"docker volume create ${C}v >/dev/null
docker volume rm ${C}v >/dev/null
docker volume ls --format "{{.Name}}" | grep -q "^${C}v$" && echo STILL || echo VOL_REMOVED"#).contains("VOL_REMOVED"),
            host("dockervol/inspect", r"docker volume create ${C}v >/dev/null
docker volume inspect ${C}v >/dev/null 2>&1 && echo VOL_INSPECT_OK
docker volume rm ${C}v >/dev/null 2>&1").contains("VOL_INSPECT_OK"),
            host("dockervol/tmpfs-fresh", r#"docker run --rm $PLAT --tmpfs /cache $IMG sh -c "echo hi > /cache/f && cat /cache/f && ls -1 /cache | wc -l | tr -d ' '"
docker run --rm $PLAT --tmpfs /cache $IMG sh -c "ls -1 /cache | wc -l | tr -d ' '""#).contains("hi").contains("1").contains("0"),
            host("dockervol/mount-tmpfs", r#"docker run -d --name ${C}c $PLAT --mount type=tmpfs,destination=/scratch $IMG sleep 60 >/dev/null; sleep 0.4
docker exec ${C}c sh -c "echo ok > /scratch/f && cat /scratch/f"
docker inspect -f "{{range .Mounts}}{{.Type}} {{.Destination}}{{end}}" ${C}c"#).contains("ok").contains("tmpfs /scratch"),
            host("dockervol/anon-volume", r#"docker run -d --name ${C}c $PLAT -v /data $IMG sh -c "echo ANON > /data/f; sleep 60" >/dev/null; sleep 0.6
docker exec ${C}c cat /data/f
docker inspect -f "{{range .Mounts}}{{.Type}}:{{.Destination}} {{end}}" ${C}c | grep -q "volume:/data" && echo ANON_IS_VOLUME"#).contains("ANON").contains("ANON_IS_VOLUME"),
            host("dockervol/mount-volume-inuse", r#"docker volume create ${C}v >/dev/null
docker run -d --name ${C}c $PLAT --mount type=volume,source=${C}v,destination=/data $IMG sleep 60 >/dev/null; sleep 0.5
docker volume rm ${C}v 2>&1 | grep -qi "in use\|cannot" && echo IN_USE
docker rm -f ${C}c >/dev/null; docker volume rm ${C}v >/dev/null 2>&1"#).contains("IN_USE"),
            host("dockervol/subpath", "docker volume create ${C}v >/dev/null\ndocker run --rm -v ${C}v:/seed $PLAT $IMG sh -c 'mkdir -p /seed/safe && echo SUBPATH_OK > /seed/safe/value'\ndocker run --rm --mount type=volume,source=${C}v,destination=/data,volume-subpath=safe,readonly $PLAT $IMG cat /data/value").contains("SUBPATH_OK"),
            host("dockervol/subpath-missing", "docker volume create ${C}v >/dev/null\n! docker run --rm --mount type=volume,source=${C}v,destination=/data,volume-subpath=missing $PLAT $IMG true >/dev/null 2>&1 && echo SUBPATH_MISSING_OK").contains("SUBPATH_MISSING_OK"),
            host("dockervol/subpath-symlink-escape", "docker volume create ${C}v >/dev/null\ndocker run --rm -v ${C}v:/seed $PLAT $IMG ln -s /tmp /seed/escape\n! docker run --rm --mount type=volume,source=${C}v,destination=/data,volume-subpath=escape $PLAT $IMG true >/dev/null 2>&1 && echo SUBPATH_ESCAPE_OK").contains("SUBPATH_ESCAPE_OK"),
            host("dockervol/bind-private-recursive-ro", "mkdir -p $WORK/nested\ndocker run --rm --mount type=bind,source=$WORK,destination=/data,readonly,bind-propagation=private $PLAT $IMG sh -c 'echo x > /data/nested/x 2>/dev/null || echo RECURSIVE_RO_OK'").contains("RECURSIVE_RO_OK"),
            host("dockervol/bind-shared-reject", "! docker create --mount type=bind,source=$WORK,destination=/data,bind-propagation=rshared $PLAT $IMG true >/dev/null 2>&1 && echo SHARED_REJECT_OK").contains("SHARED_REJECT_OK"),
            host("dockervol/bind-nonrecursive-reject", "! docker create --mount type=bind,source=$WORK,destination=/data,bind-nonrecursive $PLAT $IMG true >/dev/null 2>&1 && echo NONRECURSIVE_REJECT_OK").contains("NONRECURSIVE_REJECT_OK"),
            host("dockervol/local-bind", "mkdir -p $WORK/device; echo LOCAL_BIND_OK > $WORK/device/value\ndocker volume create --driver local --opt type=none --opt o=bind --opt device=$WORK/device ${C}v >/dev/null\ndocker run --rm $PLAT -v ${C}v:/data $IMG cat /data/value\ndocker volume rm ${C}v >/dev/null; test -f $WORK/device/value").contains("LOCAL_BIND_OK"),
            host("dockervol/local-bind-inspect", "mkdir -p $WORK/device\ndocker volume create --driver local --opt type=none --opt o=bind,ro --opt device=$WORK/device ${C}v >/dev/null\ndocker volume inspect -f '{{.Driver}} {{.Scope}} {{index .Options \"type\"}} {{index .Options \"o\"}}' ${C}v\ndocker volume rm ${C}v >/dev/null").contains("local local none bind,ro"),
            host("dockervol/local-filesystem-reject", "mkdir -p $WORK/device\n! docker volume create --driver local --opt type=ext4 --opt o=bind --opt device=$WORK/device ${C}v >/dev/null 2>&1 && echo LOCAL_FS_REJECT_OK").contains("LOCAL_FS_REJECT_OK"),
        ],
    )
}
