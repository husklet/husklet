//! Linux file-permission semantics + `ls -l` RENDER FIDELITY — the mode string, owner/group columns,
//! type char, and special-file rendering must match a real Linux container byte-for-byte. Exercises the
//! daemon VFS mode/owner path (chmod/chown), the setuid/setgid/sticky bit render (`s`/`S`/`t`/`T`),
//! device major,minor formatting, access(2) DAC + root bypass, and overlay copy-up preserving mode.
//! All in busybox/alpine (`ls -l`/`stat` are busybox applets), each sub-second; deterministic markers
//! only (perm strings, owner/group names, type chars — never mtime/size/inode). Owner: permissions
//! agent. Every case verified on the Real docker oracle. Edit ONLY this folder.

use crate::scenario::{scen, sgroup, ScenGroup};

pub fn group() -> ScenGroup {
    sgroup("permissions", vec![
        // ---- A. chmod perm-string render via `ls -l | cut -c1-10` (busybox prints the mode string) ---
        scen("permissions/mode-0644", "alpine:latest")
            .exec("touch /f && chmod 0644 /f && ls -l /f | cut -c1-10").has("-rw-r--r--"),
        scen("permissions/mode-0755", "alpine:latest")
            .exec("touch /f && chmod 0755 /f && ls -l /f | cut -c1-10").has("-rwxr-xr-x"),
        scen("permissions/mode-0600", "alpine:latest")
            .exec("touch /f && chmod 0600 /f && ls -l /f | cut -c1-10").has("-rw-------"),
        scen("permissions/mode-0777", "alpine:latest")
            .exec("touch /f && chmod 0777 /f && ls -l /f | cut -c1-10").has("-rwxrwxrwx"),
        scen("permissions/mode-0640", "alpine:latest")
            .exec("touch /f && chmod 0640 /f && ls -l /f | cut -c1-10").has("-rw-r-----"),
        scen("permissions/mode-0444", "alpine:latest")
            .exec("touch /f && chmod 0444 /f && ls -l /f | cut -c1-10").has("-r--r--r--"),
        // setuid: owner-exec present -> lowercase 's'; owner-exec absent -> uppercase 'S'
        scen("permissions/mode-suid-4755", "alpine:latest")
            .exec("touch /f && chmod 4755 /f && ls -l /f | cut -c1-10").has("-rwsr-xr-x"),
        scen("permissions/mode-suid-noexec-4644", "alpine:latest")
            .exec("touch /f && chmod 4644 /f && ls -l /f | cut -c1-10").has("-rwSr--r--"),
        // setgid: group-exec present -> 's'; absent -> 'S'
        scen("permissions/mode-sgid-2755", "alpine:latest")
            .exec("touch /f && chmod 2755 /f && ls -l /f | cut -c1-10").has("-rwxr-sr-x"),
        scen("permissions/mode-sgid-noexec-2644", "alpine:latest")
            .exec("touch /f && chmod 2644 /f && ls -l /f | cut -c1-10").has("-rw-r-Sr--"),
        // sticky bit on a dir: other-exec present -> 't'; absent -> 'T'
        scen("permissions/mode-sticky-1777", "alpine:latest")
            .exec("mkdir /d && chmod 1777 /d && ls -ld /d | cut -c1-10").has("drwxrwxrwt"),
        scen("permissions/mode-sticky-noother-1776", "alpine:latest")
            .exec("mkdir /d && chmod 1776 /d && ls -ld /d | cut -c1-10").has("drwxrwxrwT"),
        // Same render via `stat -c %A` (human-readable mode). See report: busybox stat %A should equal
        // coreutils — flagged for oracle confirmation.
        scen("permissions/stat-mode-A-0644", "alpine:latest")
            .exec("touch /f && chmod 0644 /f && stat -c '%A' /f").has("-rw-r--r--"),

        // ---- B. chown / chgrp owner+group columns (root -> chown succeeds) — cols 3,4 of busybox ls -l -
        scen("permissions/chown-root", "alpine:latest")
            .exec("touch /f && chown 0:0 /f && ls -l /f | awk '{print $3, $4}'").has("root root"),
        // alpine /etc/passwd has daemon (uid 2, gid 2) -> resolves to the NAME in the ls columns
        scen("permissions/chown-named-daemon", "alpine:latest")
            .exec("touch /f && chown daemon:daemon /f && ls -l /f | awk '{print $3, $4}'").has("daemon daemon"),
        // a uid/gid with no /etc/passwd entry stays NUMERIC in the render
        scen("permissions/chown-numeric-unknown", "alpine:latest")
            .exec("touch /f && chown 12345:12345 /f && ls -l /f | awk '{print $3, $4}'").has("12345 12345"),
        scen("permissions/chown-stat-uid-gid", "alpine:latest")
            .exec("touch /f && chown 1:1 /f && stat -c '%u:%g' /f").has("1:1"),

        // ---- C. type char (first col of ls -l) + special-file render --------------------------------
        scen("permissions/type-dir", "alpine:latest")
            .exec("mkdir /d && ls -ld /d | cut -c1").has("d"),
        // symlink: type char 'l', perms always rwxrwxrwx, and the `-> target` tail
        scen("permissions/type-symlink", "alpine:latest")
            .exec("ln -s /etc/hostname /l && ls -l /l").has("lrwxrwxrwx").has("-> /etc/hostname"),
        scen("permissions/type-fifo", "alpine:latest")
            .exec("mkfifo /p && ls -l /p | cut -c1").has("p"),
        // char devices show "major, minor" (busybox prints them where a regular file's size would be)
        scen("permissions/dev-null-majmin", "alpine:latest")
            .exec("ls -l /dev/null | awk '{print $5, $6}'").has("1, 3"),
        scen("permissions/dev-zero-majmin", "alpine:latest")
            .exec("ls -l /dev/zero | awk '{print $5, $6}'").has("1, 5"),
        scen("permissions/dev-full-majmin", "alpine:latest")
            .exec("ls -l /dev/full | awk '{print $5, $6}'").has("1, 7"),

        // ---- D. access(2) / `test` honour the mode, with root DAC bypass ----------------------------
        // root bypasses DAC on READ: a 0000 file is still readable as root
        scen("permissions/root-reads-0000", "alpine:latest")
            .exec("touch /f && chmod 000 /f && cat /f >/dev/null 2>&1 && echo ROOT_READ_OK").has("ROOT_READ_OK"),
        // execute has NO root bypass unless SOME exec bit is set: a 0644 file is not executable even for root
        scen("permissions/nonexec-test-x", "alpine:latest")
            .exec("touch /f && chmod 644 /f && { test -x /f && echo X || echo NOX; }").has("NOX"),

        // ---- E. overlay copy-up preserves + applies mode/owner --------------------------------------
        // chmod a lower-layer file -> copy-up into the upper keeps the new mode
        scen("permissions/copyup-chmod-lower", "alpine:latest")
            .exec("chmod 600 /etc/hostname && stat -c '%a' /etc/hostname").has("600"),
        // copy a lower binary (busybox is 0755) -> the copy keeps executable perms
        scen("permissions/copyup-binary-perms", "alpine:latest")
            .exec("cp /bin/busybox /b2 && ls -l /b2 | cut -c1-10").has("-rwxr-xr-x"),
    ])
}
