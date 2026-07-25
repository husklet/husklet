//! Typed bind-volume contracts.

use crate::contract::{Group, Scenario};

fn host(id: &'static str, command: &str) -> Scenario {
    Scenario::new(id, "alpine:latest").host(command)
}

pub fn group() -> Group {
    Group::new(
        "volumes",
        vec![
            host("volumes/write-seen-on-host", "docker run --rm $PLAT -v \"$WORK\":/data $IMG sh -c 'echo CWROTE > /data/f.txt'\ncat \"$WORK/f.txt\"").contains("CWROTE"),
            host("volumes/host-seen-in-container", "echo HSEED > \"$WORK/h.txt\"\ndocker run --rm $PLAT -v \"$WORK\":/data $IMG cat /data/h.txt").contains("HSEED"),
            host("volumes/readonly-rejects-write", "docker run --rm $PLAT -v \"$WORK\":/data:ro $IMG sh -c 'echo x > /data/n 2>/dev/null || echo RO_REJECTED'").contains("RO_REJECTED"),
            host("volumes/delete-propagates", "echo a > \"$WORK/d.txt\"\ndocker run --rm $PLAT -v \"$WORK\":/data $IMG rm /data/d.txt\ntest ! -e \"$WORK/d.txt\" && echo DELETED").contains("DELETED"),
            host("volumes/persist-across-runs", "docker run --rm $PLAT -v \"$WORK\":/data $IMG sh -c 'echo persisted > /data/p'\ndocker run --rm $PLAT -v \"$WORK\":/data $IMG cat /data/p").contains("persisted"),
            host("volumes/subdir-mount", "mkdir -p \"$WORK/sub\"\necho inner > \"$WORK/sub/inner.txt\"\ndocker run --rm $PLAT -v \"$WORK/sub\":/mnt $IMG sh -c 'cat /mnt/inner.txt && ls /mnt'").contains("inner.txt").contains("inner"),
            host("volumes/two-mounts", "mkdir -p \"$WORK/m1\" \"$WORK/m2\"\necho one > \"$WORK/m1/a\"\necho two > \"$WORK/m2/b\"\ndocker run --rm $PLAT -v \"$WORK/m1\":/x -v \"$WORK/m2\":/y $IMG sh -c 'cat /x/a; cat /y/b'").contains("one").contains("two"),
            host("volumes/nested-dotdot-crosses-boundary", "mkdir -p \"$WORK/sub\"\necho host-sibling > \"$WORK/HOSTMARK\"\ndocker run --rm $PLAT -v \"$WORK/sub\":/mnt $IMG sh -c 'ls /mnt/.. | grep -qx etc && ls /mnt/.. | grep -qx bin && echo PARENT_IS_ROOTFS; ls /mnt/.. | grep -q HOSTMARK && echo LEAKED || echo NO_LEAK'").contains("PARENT_IS_ROOTFS").contains("NO_LEAK"),
            host("volumes/cmd-cat-grep-wc", "printf 'apple\\nbanana\\ncherry\\n' > \"$WORK/f\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'cat /d/f | grep a | wc -l | tr -d \" \"'").contains("2"),
            host("volumes/cmd-cp-mv-rm", "echo hi > \"$WORK/a\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'cp /d/a /d/b && mv /d/b /d/c && rm /d/a && ls /d | sort | tr \"\\n\" \",\"'").contains("c,"),
            host("volumes/cmd-sed-inplace", "echo 'foo=1' > \"$WORK/cfg\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'sed -i s/foo/bar/ /d/cfg'\ncat \"$WORK/cfg\"").contains("bar=1"),
            host("volumes/cmd-append-redirect", "echo one > \"$WORK/log\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'echo two >> /d/log'\ntr \"\\n\" \" \" < \"$WORK/log\"").contains("one two"),
            host("volumes/cmd-sort-head-tail", "printf '3\\n1\\n2\\n' > \"$WORK/n\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'echo MIN=$(sort /d/n | head -1); echo MAX=$(sort /d/n | tail -1)'").contains("MIN=1").contains("MAX=3"),
            host("volumes/cmd-chmod-perms", "echo x > \"$WORK/s\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'chmod 640 /d/s && ls -l /d/s | cut -c1-10'").contains("-rw-r-----"),
            host("volumes/cmd-mkdir-touch-find", "docker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'mkdir -p /d/x/y && touch /d/x/y/z.txt && find /d -name z.txt'").contains("/d/x/y/z.txt"),
            host("volumes/cmd-wc-bytes", "printf 'abcde' > \"$WORK/b\"\ndocker run --rm $PLAT -v \"$WORK\":/d $IMG sh -c 'wc -c < /d/b | tr -d \" \"'").contains("5"),
        ],
    )
}
