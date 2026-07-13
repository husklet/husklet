# JIT fd/thread/default-off allocation audit — wave P (2026-07)

Scope: exact `DD_NFD` arrays, thread registries, checkpoint, forkserver, and untrusted-sentry storage. Sizes assume 64-bit pointers/`size_t`, 4-byte `int`, and source-declared element widths; platform structs such as `termios` require a linker map/`sizeof` probe before implementation. No code was changed.

## Result

`DD_NFD=65,536` is a compatibility range, not an allocation requirement. The current implementation uses the fd number as an index into dozens of independent arrays. Known fixed-width/path arrays exceed 65 MiB before platform structs and dynamically allocated contents. The comment in `container/state.c` calling this “a few MB of never-resident address space” is false: zero BSS may initially share pages, but normal close/reset loops and scattered feature writes dirty pages across many arrays.

Do not restore the old 1,024 limit: Chromium already proved high-number fds are real. Keep bounds checks at 65,536 (or configured `RLIMIT_NOFILE`) and make uncommon feature metadata sparse/lazy. Dense storage remains appropriate for the core host-fd/flags mapping if it is touched on nearly every fd syscall.

## Dense fd ledger

### Dominant pathname reservations

| Array | Exact source bytes | Use/lookup |
|---|---:|---|
| `g_fdpath[DD_NFD][192]` | 12,582,912 | synthetic `/proc/self/fd`, reopen/path identity; indexed on fd lifecycle/path queries |
| `g_unix_bind[DD_NFD][108]` | 7,077,888 | Unix socket bound pathname |
| `g_inotify_wpath[DD_NFD][512]` | 33,554,432 | inotify watch path, although watches are uncommon |
| `g_proc_text_desc[DD_NFD][64]` | 4,194,304 | synthetic proc-text descriptor |
| `g_ovldir[1024][192]` | 196,608 | deliberately limited overlay-directory compatibility table |

The first four reserve 57,409,536 bytes. Convert them to optional heap strings owned by one per-fd metadata record. This is behavior-neutral: empty string remains NULL; high fd numbers remain valid. It also eliminates fixed pathname truncation at 107/191/511/63 bytes if allocation stores the validated full path. If truncation is observable today, changing it is a compatibility improvement but requires path-length tests.

### Exact primitive/pointer arrays

The following sizes exclude platform-dependent structs:

- pipe size `int`: 256 KiB;
- flock type byte: 64 KiB;
- lock dev + inode + valid (`dev_t`/`ino_t` assumed 8 bytes): 1,088 KiB;
- epoll deferred pointer/count/cap, state bytes, owner/events/udata/OFD id: about 2.8 MiB;
- epoll prime pointer/count/cap, wake/mojo bytes, membership pointer: about 1.7 MiB;
- signalfd reverse byte: 64 KiB;
- lease/fsig/dnotify mask/signal: 448 KiB;
- fd container port: 128 KiB;
- timerfd deadline/interval/clock/flags: 1,376 KiB;
- pushback pointer + length: 1 MiB (payload already dynamic);
- memf pointer: 512 KiB;
- eventfd peer/slot/refs/semaphore: 832 KiB;
- PTY index/master flags: 320 KiB, plus dense termios/winsize storage;
- proc/dev/memfd/inotify/timer/pagemap/GPU type flags and seals/owners: over 1 MiB combined.

Network namespace/socket tracking adds roughly 4–5 MiB: port/family/state bytes, pair/peer ids, TCP IPv4/IPv6 addresses (the IPv6 address array alone is 1 MiB), bridge/netlink/DNS peer data, and local/listen/passcred state.

Initialization is mostly free zero-fill, but feature cleanup, dup/fork propagation and fd reuse touch multiple arrays. Lookups are O(1) direct indexing. A sparse replacement must retain O(1) for common operations and must not heap-allocate on every read/write.

## Sparse design without syscall regression

Use a two-level fd directory: 256 pages × 256 fds (or 64 × 1,024), with a NULL top-level entry meaning all-default. Allocate a compact page on the first non-default feature for any fd in that page. Direct lookup is two array accesses and one predictable NULL branch; common plain file/socket operations can keep the existing core fd mapping fast. Store large strings and epoll changelists behind pointers.

Split metadata by frequency:

1. **core dense or paged:** real-host-fd ownership, close-on-exec/dup identity, minimal type bits used on every syscall;
2. **socket page:** family/state/ports/addresses/peers, allocated at `socket`/`accept`/`socketpair`;
3. **epoll page/object:** epoll instance and watched-fd data, allocated at `epoll_create`/`epoll_ctl`;
4. **special-fd object:** eventfd/timerfd/signalfd/inotify/memfd/PTY/proc synthetic state;
5. **path object:** optional full strings.

Do not use a single hash table guarded by one global mutex: it adds contention and unpredictable lookup to every syscall. Page pointers can be process-local and protected by the existing fd lifecycle lock; fork naturally COW-copies pages. Release an empty page only at safe lifecycle points rather than on every close.

## Risky limits versus neutral allocation

Behavior-neutral:

- lazy/page allocation while preserving `fd < DD_NFD`;
- optional strings replacing empty fixed buffers;
- allocating epoll/socket/special metadata only when the fd acquires that type;
- lazy checkpoint/forkserver buffers and sentry process tables;
- merging duplicate thread registries without changing the 4,096 accepted-thread ceiling.

Behavior-changing and not authorized by memory cleanup:

- lowering `DD_NFD`, `SENTRY_VFD_MAX`, thread/process counts, checkpoint fd/process caps, or forkserver live/argv/env caps;
- changing current overflow errno/drop behavior;
- increasing path preservation beyond current truncation without compatibility tests;
- making sentry’s 1,024 virtual-fd cap appear to support the trusted engine’s 65,536 range. The comment “far beyond any test/jail guest” is not a contract.

## Thread registries

`g_stw_threads[4096]` has `{atomic int, pthread_t, atomic u64}` and is approximately 24 bytes/slot = 96 KiB. `g_threg[4096]` has four pointers (`cpu`, thread, wait condvar, wait mutex) = 32 bytes/slot = 128 KiB. Retired and freed cache records are 24 bytes each: `(4096+8)` + 4096 records, about 192 KiB. Total fixed thread/reclamation records are about 416 KiB.

Both registries linearly scan 4,096 slots for registration, unregister, targeted signal, peer count, stop-the-world signaling and generation checks. That CPU cost matters more than BSS. Give each live thread one allocated record containing CPU/thread/wait pointers, STW `used/exec_gen`, and a stable slot/token; retain an intrusive live list plus tid index. Dispatcher publication remains one TLS-pointer atomic store and must not gain a lookup. STW snapshot can walk only live records under the existing registry lock.

Merging is behavior-neutral only if registration is atomic across both responsibilities. Current `g_threg` overflow can drop targeted-signal mapping while STW registration independently succeeds; preserve documented errors or improve them explicitly. Test fork-child registry reset, thread exit concurrent with cache flush/tgkill, futex interruption, 4,096-thread boundary, and stale-record reclamation.

## Default-off allocations

### Checkpoint/restore

Always-linked static data includes:

- `zero[65536]`: 64 KiB;
- `fdrecs[1024]`: each `ckpt_fd` is 536 bytes (`4×i32 + i64 + char[512]`) = 548,864 bytes;
- `foll[512]`: 2 KiB;
- `g_rprocs[512]`: four ints = 8 KiB;
- checkpoint directory/path scratch: several KiB.

About 625 KiB is reserved when checkpointing is off. Allocate fd records and restore process records only after `DDJIT_CHECKPOINT_DIR`/`DDJIT_RESTORE_DIR` activates. Use a small streaming zero buffer on stack or one lazily allocated page. No normal syscall path should change; `ckpt_poll` already has a separate activation gate.

### Forkserver

Two distinct function-static `FSRV_BUFSZ=256 KiB` buffers reserve 512 KiB. `g_fsrv_live[256]` is 8 bytes/entry = 2 KiB, plus argv/env pointer vectors and path stores. Allocate one receive/pack buffer per entered server loop, or reuse one server-owned buffer because those paths are not concurrent in one thread. Ordinary direct launches then reserve none. Preserve the 256-KiB wire limit and 256 live-launch limit unless separately revising protocol behavior.

### Untrusted sentry

The shared ring payload is already dynamically mapped only when enabled: eight rings each contain a 1-MiB buffer, so activation costs a little over 8 MiB. That is correct lazy behavior. Always-linked `g_proc[64]` is approximately 64 × 6,152 bytes = 393,728 bytes (`real[1024]`, two byte arrays, ids/flags), plus tiny control arrays. The per-thread `piov[1024]` is 16 KiB TLS when touched; pselect scratch is 387 bytes.

Move `g_proc` behind sentry initialization, but do not change ring allocation or inline it into normal engine state. A dynamically allocated 64-entry table keeps identical lookup/locking and zero syscall latency after activation. Separately reconcile `SENTRY_VFD_MAX=1024` with high-fd compatibility; that is behavior work, not a lazy-allocation patch.

## Maximal implementation groups

1. **P1 path/special metadata:** optional strings plus paged special-fd state. Expected static reduction at least 57.4 MiB, likely over 60 MiB. Preserve fd range.
2. **P2 socket/epoll pages:** group network, epoll, PTY and lock metadata by live type. Expected additional 8–12 MiB. Benchmark plain read/write/open/close, socket send/recv and epoll_ctl/wait.
3. **P3 unified live-thread records:** remove duplicate 4,096 scans and about 224 KiB registry BSS; keep TLS direct publication. Retired/freed arrays can become bounded vectors sized by actual outstanding generations.
4. **P4 default-off lazy data:** checkpoint (~625 KiB), forkserver (~514 KiB), sentry process table (~394 KiB). Roughly 1.5 MiB default reduction with no feature-off hot-path change.

## Acceptance gates

Measure linked BSS/data/TLS and launch RSS before/after, then page faults and resident pages after opening 0, 1K, 20K and 65K plain fds. Performance gates: repeated open/close/read/write/dup/fcntl, socket/accept/send/recv, epoll_ctl/wait at sparse and dense fd numbers, fork/exec with thousands of fds, and 1/64/1K-thread signal/futex/cache-flush loads. Correctness gates in Rust/C must cover high-number Chromium memfd seals, SCM_RIGHTS, fd reuse, all special fd families, `/proc/self/fd`, Unix/inotify maximum paths, CLOEXEC, checkpoint 1,024-fd boundary, forkserver maximum packet/live launches, sentry fork/exec and its explicit 1,024 limit. Allocation-failure paths must return Linux-compatible errors without corrupting the core fd table.
