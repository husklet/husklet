# Chrome peer-procfs env switches

Two environment switches shape how a guest sees a **peer** process's synthetic
`/proc/<pid>/{status,statm}` files. They exist because Chrome's memory monitor
polls its child processes' procfs, and some launch modes need to hide or reshape
that data. They are load-bearing for Chrome; do not remove them.

Both switches are read at request time in `dd-jit-darwin/src/runtime/os/linux/container/vfs.c`
(`proc_open` peer branch, `proc_chromium_memory_monitor_hidden`, `proc_peer_diag_text`)
and are forwarded to the engine by both launch paths:

- typed daemon launch — `dd-cli/src/ddjit_launcher.rs`
- out-of-process script launch — `dd-jit-darwin/src/spawn_config.rs`

## `DD_HIDE_CHROME_PROCFILES`

Hides a peer's memory-monitor procfs files. When set, an open of a peer
`/proc/<pid>/status` (and `statm`) fails with `ENOENT` instead of returning a
synthesized body.

| Value            | Effect                                                        |
|------------------|---------------------------------------------------------------|
| unset / empty    | peer `status` and `statm` are visible (normal synthesis)      |
| `status`         | only peer `status` is hidden                                  |
| `statm`          | only peer `statm` is hidden                                   |
| any other value  | both peer `status` and `statm` are hidden                     |

(Chrome-named guests — `comm` of `chromium`/`chrome`/`google-chrome`, or an exe
path containing `chrom` — are additionally subject to the memory-monitor hide.)

## `DD_PROC_CHROME_MODE`

Reshapes the *content* of a peer's `status`/`statm` instead of hiding it.

| Value                       | Effect                                                        |
|-----------------------------|---------------------------------------------------------------|
| unset / empty               | full synthesized body (live rss/state from libproc)          |
| `empty`                     | both files open but return a 0-byte body                      |
| `empty-status`/`empty-statm`| the named file returns a 0-byte body                          |
| `zero-mem`                  | canonical body with `VmRSS`/`VmSize`/resident forced to 0     |
| `linux-min` / `minimal`     | a minimal canonical Linux status/statm field set             |
| `invalid`                   | a deliberately malformed one-line body                       |
| `no-peer-task`              | hide peer `/proc/<pid>/task/<tid>` directories               |

## Test gate

The observable procfs effect of each switch is pinned by golden cases in
`dd-tests/src/cases/ext/procfs.rs` (`procfs-proc` group) driving
`dd-tests/guests/ext_procfs/chrome_procswitch.c`:

- `pf-chrome-default` — no switch: peer `status` opens, non-empty body.
- `pf-chrome-hide` — `DD_HIDE_CHROME_PROCFILES=1`: peer `status` open fails (hidden).
- `pf-chrome-empty` — `DD_PROC_CHROME_MODE=empty`: peer `status` opens, empty body.
