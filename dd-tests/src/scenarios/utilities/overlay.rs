//! overlay VFS coherence — metadata semantics (overlay_lookup fast path + updirneg memo) and the
//! positive dentry-cache coherence storm (fscache.c dc_*).

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- overlay VFS metadata semantics (guards the overlay_lookup fast path + updirneg memo) --
        // The engine short-circuits per-entry UPPER probes when a path's parent dir is provably absent
        // from the upper (a negative memo, invalidated by the shared namespace epoch). This scenario
        // exercises every way that proof can go stale: create-after-walk (same + child process),
        // whiteout after delete, upper-shadows-lower, chmod copy-up (a NON-creating syscall that
        // mutates the upper), and rm-rf+recreate (opaque dir hiding lower children).
        scen("utilities/overlay-metadata", "alpine")
            .exec("set -e; \
                   ls -laR /usr/lib >/dev/null; \
                   touch /usr/lib/NEWFILE && test -f /usr/lib/NEWFILE || { echo FAIL-create-after-walk; exit 1; }; \
                   sh -c 'echo x > /usr/lib/XPROC'; test -f /usr/lib/XPROC || { echo FAIL-xproc-create; exit 1; }; \
                   rm /etc/os-release; test ! -e /etc/os-release || { echo FAIL-whiteout; exit 1; }; \
                   ls /etc | grep -q '^os-release$' && { echo FAIL-whiteout-readdir; exit 1; }; \
                   echo SHADOW > /etc/hostname.d; :; \
                   chmod 600 /etc/passwd && [ \"$(stat -c %a /etc/passwd)\" = 600 ] || { echo FAIL-chmod-copyup; exit 1; }; \
                   grep -q root /etc/passwd || { echo FAIL-copyup-content; exit 1; }; \
                   rm -rf /usr/share/apk && mkdir /usr/share/apk && [ -z \"$(ls /usr/share/apk)\" ] || { echo FAIL-opaque; exit 1; }; \
                   echo overlay-metadata-ok")
            .has("overlay-metadata-ok"),

        // ---- positive dentry-cache coherence under the OVERLAY (guards fscache.c dc_*) ------
        // The engine memoizes successful per-directory path resolutions (the realpath climb), keyed on
        // the shared namespace epoch. This storm interleaves the overlay-specific ways a POSITIVE
        // resolution can go stale -- rename/unlink/symlink-flip in the upper, chmod copy-up (relocates
        // a lower file's host path with NO creating syscall in dispatch's bump set), rm of a lower file
        // (whiteout: the old positive path must die), recreate over the whiteout, and a lower-only dir
        // renamed in place -- with immediate lookups that would read through any stale cached path.
        scen("utilities/dentry-storm-overlay", "alpine")
            .exec("ok=1; i=0; while [ $i -lt 15 ]; do \
                     echo v$i > /tmp/f; mv /tmp/f /tmp/g; [ -e /tmp/f ] && ok=0; \
                     read w < /tmp/g; [ \"$w\" = v$i ] || ok=0; rm /tmp/g; [ -e /tmp/g ] && ok=0; \
                     echo A > /tmp/a; echo B > /tmp/b; ln -s a /tmp/l; read w < /tmp/l; [ \"$w\" = A ] || ok=0; \
                     rm /tmp/l; ln -s b /tmp/l; read w < /tmp/l; [ \"$w\" = B ] || ok=0; \
                     rm /tmp/l /tmp/a /tmp/b; i=$((i+1)); \
                   done; \
                   ls /usr/share/udhcpc >/dev/null; \
                   chmod 600 /usr/share/udhcpc/default.script || ok=0; \
                   grep -q . /usr/share/udhcpc/default.script || ok=0; \
                   rm /etc/services; [ -e /etc/services ] && ok=0; \
                   echo re-created > /etc/services; read w < /etc/services; [ \"$w\" = re-created ] || ok=0; \
                   mv /usr/share/udhcpc /usr/share/udhcpc2; [ -e /usr/share/udhcpc/default.script ] && ok=0; \
                   [ -e /usr/share/udhcpc2/default.script ] || ok=0; \
                   echo dentry-overlay ok=$ok")
            .has("dentry-overlay ok=1"),
    ]
}
