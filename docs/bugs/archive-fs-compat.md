# Archive and Filesystem Compatibility Gaps

Date: 2026-07-10

These findings came from isolated workspaces `/tmp/dd-agent5-sparse.seZJ2E` and `/Users/x/dd/dd-verify-5b`. The main worktree was not modified.

## Source-Inferred: Darwin Jail Symlink Semantics Can Produce Wrong Contents

Priority: P3
Impact: possible wrong-content behavior for macOS-container paths
Confidence: Medium

Evidence:

- Darwin jail maps guest absolute paths to host paths by string after canonicalizing `.` and `..`: `dd-jit-darwin/src/runtime/os/darwin/jail/jail.c:340`.
- Host `open`/`stat` then follows symlinks unless a specific call uses nofollow behavior.

Why this is suspicious:

Linux VFS resolution has a component-walk resolver that clamps symlinks in the guest namespace. The Darwin jail path is simpler and may silently become host symlink semantics for symlinked rootfs paths.

Verification needed:

Create a macOS-container rootfs with symlinks that point outside the root and compare `open`, `stat`, and write behavior against expected container path semantics.

Status (2026-07-10): DEFERRED — genuinely blocked on this Linux dev host. `jail.c` is
macOS-only (a DYLD-interposing arm64 dylib cross-compiled through the `mac` bridge); it
cannot be run on Linux, so the fix cannot be exercised/verified here. A correct fix is not
a one-line clamp: because the jail maps a guest path to a single host string and each libc
interposer then applies its own symlink-follow semantics (`open` follows the final
component, `lstat`/`O_NOFOLLOW` do not), containing symlink escapes requires a
component-walk resolver that clamps symlink targets into the guest namespace across every
interposer — a high-blast-radius change to the resolution path used by every jailed
syscall. Given this is the only P3, source-inferred, unproven finding and the change cannot
be runtime-validated on this host, it is intentionally left for a macOS-hosted change that
can build and test the jail (real distro rootfs with intra- and cross-root symlinks,
asserting `open`/`stat`/write are clamped) before shipping.

