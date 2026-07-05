//! Filesystem WITHOUT a volume — the container's own rootfs + overlay upper. Exercises the daemon VFS:
//! path resolution (`.`/`..`, relative vs absolute), readdir/stat/readlink, mkdir/rm/rename, permission
//! bits (chmod/chown), symlinks (create/follow/dangling), and overlay copy-up (modify a lower-layer file
//! → it lands in the upper). All in busybox/alpine, each sub-second; deterministic markers. Owner: fs
//! agent. Every case verified on the Real docker oracle. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup};

pub fn group() -> ScenGroup {
    sgroup("filesystem", vec![
        // ---- listing / traversal --------------------------------------------------------------------
        scen("filesystem/ls-rootfs", "alpine:latest")
            .exec("cd / && ls | grep -qx etc && ls | grep -qx bin && echo LS_OK").has("LS_OK"),
        scen("filesystem/cd-absolute", "alpine:latest")
            .exec("cd /usr/bin && pwd").has("/usr/bin").plus_mac(),
        scen("filesystem/cd-relative", "alpine:latest")
            .exec("cd /usr && cd bin && pwd").has("/usr/bin").plus_mac(),
        scen("filesystem/dotdot-ascends", "alpine:latest")
            .exec("mkdir -p /t/a/b && cd /t/a/b && cd ../.. && pwd").has("/t"),
        // REGRESSION (darwinjail cwd): `cd..` must actually ASCEND, not stay in the same folder.
        // Uses only paths present in BOTH a Linux image and the macOS container, so it exercises the mac
        // container `cd` too — `.plus_mac()` (runs on `-t mac`; the default suite stays Linux-only).
        scen("filesystem/cd-dotdot-existing", "alpine:latest")
            .exec("cd /usr/bin && cd .. && pwd | grep -qx /usr && echo CD_DOTDOT_OK").has("CD_DOTDOT_OK").plus_mac(),
        scen("filesystem/dot-stays", "alpine:latest")
            .exec("cd /etc && cd . && pwd").has("/etc").plus_mac(),
        scen("filesystem/dotdot-to-root", "alpine:latest")
            .exec("cd /usr/lib && cd ../.. && pwd | grep -qx / && echo AT_ROOT").has("AT_ROOT"),
        scen("filesystem/find-by-name", "alpine:latest")
            .exec("mkdir -p /t/x/y && touch /t/x/y/needle.txt && find /t -name needle.txt").has("/t/x/y/needle.txt"),
        scen("filesystem/find-type-d", "alpine:latest")
            .exec("mkdir -p /t/d1 /t/d2 && find /t -mindepth 1 -type d | sort | tr '\\n' ' '").has("/t/d1 /t/d2"),

        // ---- stat / readlink ------------------------------------------------------------------------
        scen("filesystem/stat-size", "alpine:latest")
            .exec("printf 'hello' > /f && stat -c %s /f").has("5"),
        scen("filesystem/stat-type", "alpine:latest")
            .exec("mkdir -p /d && stat -c %F /d").has("directory"),
        scen("filesystem/readlink", "alpine:latest")
            .exec("ln -s /etc/hostname /lnk && readlink /lnk").has("/etc/hostname"),

        // ---- mkdir / rm / rename --------------------------------------------------------------------
        scen("filesystem/mkdir-rmdir", "alpine:latest")
            .exec("mkdir -p /t/a && test -d /t/a && echo MADE && rm -r /t/a && test ! -e /t/a && echo REMOVED").has("MADE").has("REMOVED"),
        scen("filesystem/rename-file", "alpine:latest")
            .exec("echo val > /a && mv /a /b && cat /b && test ! -e /a && echo MOVED").has("val").has("MOVED"),
        scen("filesystem/rename-dir", "alpine:latest")
            .exec("mkdir -p /src && echo x > /src/f && mv /src /dst && cat /dst/f && test ! -e /src && echo DMOVED").has("DMOVED"),
        scen("filesystem/deep-mkdir", "alpine:latest")
            .exec("mkdir -p /a/b/c/d/e && echo deep > /a/b/c/d/e/f && cat /a/b/c/d/e/f").has("deep"),

        // ---- permissions (chmod / chown) ------------------------------------------------------------
        scen("filesystem/chmod-bits", "alpine:latest")
            .exec("touch /f && chmod 640 /f && stat -c %a /f").has("640"),
        scen("filesystem/chmod-exec-runs", "alpine:latest")
            .exec("printf '#!/bin/sh\\necho SCRIPT_RAN\\n' > /s && chmod +x /s && /s").has("SCRIPT_RAN"),
        scen("filesystem/chown-uid-gid", "alpine:latest")
            .exec("touch /f && chown 1:1 /f && stat -c '%u:%g' /f").has("1:1"),
        // a process that DROPS privilege at runtime (gosu setuid/setgid to postgres=70:70) and then
        // creates a file/dir must own the new inode with its CURRENT euid/egid, not the container id (0).
        // Regressed initdb ("data directory has wrong ownership"). Covers file AND dir, uid AND gid.
        scen("filesystem/setuid-drop-owns-newfile", "postgres:16-alpine")
            .exec("gosu postgres sh -c 'mkdir /td && touch /td/f && stat -c %u:%g /td/f && stat -c %u:%g /td'")
            .has("70:70"),

        // ---- symlinks (create / follow / dangling) --------------------------------------------------
        scen("filesystem/symlink-follow", "alpine:latest")
            .exec("echo data > /real && ln -s /real /link && cat /link").has("data"),
        scen("filesystem/symlink-is-link", "alpine:latest")
            .exec("echo x > /real && ln -s /real /link && test -L /link && echo IS_LINK").has("IS_LINK"),
        scen("filesystem/symlink-dangling", "alpine:latest")
            .exec("ln -s /no/such/target /d && test -L /d && readlink /d && { cat /d 2>&1 || echo DANGLING; }").has("/no/such/target").has("DANGLING"),
        scen("filesystem/symlink-relative", "alpine:latest")
            .exec("mkdir -p /t && echo rel > /t/target && ln -s target /t/link && cat /t/link").has("rel"),

        // ---- overlay copy-up: modify a lower-layer file -> lands in upper, re-read ------------------
        scen("filesystem/overlay-copyup", "alpine:latest")
            .exec("cat /etc/alpine-release > /dev/null && echo COPYUP_MARK >> /etc/hostname && cat /etc/hostname | grep -q COPYUP_MARK && echo COPYUP_OK").has("COPYUP_OK"),
        scen("filesystem/overlay-newfile-upper", "alpine:latest")
            .exec("echo upper > /upper-file && sync && cat /upper-file").has("upper"),
        scen("filesystem/lower-read", "alpine:latest")
            .exec("test -f /etc/os-release && grep -q Alpine /etc/os-release && echo LOWER_READ_OK").has("LOWER_READ_OK"),

        // ---- busybox parity (musl-light) ------------------------------------------------------------
        scen("filesystem/busybox-traversal", "busybox:latest")
            .exec("mkdir -p /t/a/b/c && cd /t/a/b/c && cd ../../.. && pwd").has("/t"),
        scen("filesystem/busybox-symlink", "busybox:latest")
            .exec("echo bb > /r && ln -s /r /l && cat /l").has("bb"),

        // ---- device nodes (/dev) + generated /etc + magic /proc symlinks ----------------------------
        // The container's /dev is a fresh set of standard nodes (runc mounts a tmpfs); dd synthesizes them.
        scen("filesystem/dev-null", "alpine:latest")
            .exec("echo discard > /dev/null && head -c1 /dev/null | wc -c").has("0"),
        scen("filesystem/dev-zero", "alpine:latest")
            .exec("head -c8 /dev/zero | tr '\\0' '0'").has("00000000"),
        // /dev/full: reads are zeros, but every write fails ENOSPC (Docker/runc semantics).
        scen("filesystem/dev-full-enospc", "alpine:latest")
            .exec("if echo x 2>/dev/null > /dev/full; then echo WROTE; else echo FULL_ENOSPC; fi").has("FULL_ENOSPC"),
        // /dev/shm is a writable shared tmpfs: a file created there reads back.
        scen("filesystem/dev-shm-rw", "alpine:latest")
            .exec("echo shmdata > /dev/shm/t && cat /dev/shm/t").has("shmdata"),
        // /proc/self/root -> "/", /proc/self/cwd -> the process cwd (magic symlinks).
        scen("filesystem/proc-self-root", "alpine:latest")
            .exec("readlink /proc/self/root").has("/"),
        scen("filesystem/proc-self-cwd", "alpine:latest")
            .exec("cd /usr && readlink /proc/self/cwd").has("/usr"),
        // /etc/hostname is generated beside /etc/hosts and agrees with gethostname()/`hostname`.
        scen("filesystem/etc-hostname", "alpine:latest")
            .exec("test \"$(cat /etc/hostname)\" = \"$(hostname)\" && echo HN_MATCH").has("HN_MATCH"),
        scen("filesystem/etc-files-present", "alpine:latest")
            .exec("test -f /etc/hosts && test -f /etc/resolv.conf && test -f /etc/hostname && echo ETC_OK").has("ETC_OK"),
    ])
}
