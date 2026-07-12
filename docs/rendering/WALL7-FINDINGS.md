# Wall 7 — refined localization (multi-process Chrome content blank)

Analysis-only pass (2026-07-12). No engine code changed. Builds on the exoneration
work in `render/mojo-delivery`, `render/mojo-bootstrap`, and the peer-cred tracer
(`c1c4e79b`). Grounds every claim in the captured live logs + thread samples under
`target-chrome-codex/`.

## 1. Two premise corrections (these reframe the whole hunt)

**(a) The sandbox is OFF in the failing configuration.** Every live run launches Chrome
with `--no-sandbox --disable-gpu-sandbox --disable-seccomp-filter-sandbox
--disable-setuid-sandbox` (see the `=== FLAGS:` line in any `target-chrome-codex/*.log`,
e.g. `mojo-diag.log:14`). So the child bring-up does **not** exercise dd's seccomp
(cBPF), user-namespace, or setuid-sandbox emulation. The hypothesis that "the GPU child
connects but the renderer/network children don't *because they are sandboxed*" does not
apply — there is no sandbox on any child here. dd's `unshare`/`setns` are only
flag-validated no-ops (`proc.c:561-572`), and seccomp is disabled by flag.

**(b) The GPU is IN-PROCESS in the current config** (`--in-process-gpu`). The browser
process hosts the GPU/viz thread and composites the UI — which is exactly why the UI
renders live while content does not. There is no separate GPU *process* to "connect" in
this config (older runs such as `chromium-run-gpu_retry_211107` did spawn `--type=gpu-process`;
configs vary). The renderer↔GPU command-buffer path is still **cross-process**
(renderer process ↔ browser-hosted GPU thread), so "Wall 7 = cross-process command buffer"
still holds.

## 2. The wall has MOVED across engine builds (this is the key new fact)

- **Jul-09 (`sample-renderer-23306.txt`): the renderer is 100% dormant.** All 9 guest
  threads parked for the entire 888-sample window — 7 in `svc_proc → futex_op`
  (guest FUTEX_WAIT), 2 in `svc_event → kevent` (the guest epoll/Mojo pumps). Zero JIT
  execution. The renderer never received the inbound traffic that starts it.

- **The SO_PASSCRED / SCM_CREDENTIALS synthesis fix moved the wall.** macOS AF_UNIX has
  no `SO_PASSCRED`/`SCM_CREDENTIALS`; Chromium's Mojo `NodeChannel` sets `SO_PASSCRED` and
  **aborts the bootstrap with "missing credentials"** if the first message carries none
  (`netns.c:608-612,1303-1315`). Before the synth, the renderer aborted into the dormant
  state above. After it (peer-cred tracer run `chromium-run-gpu_retry_182747/launch.log`,
  Jul-11), the renderer **exchanges hundreds of node-channel messages both ways**, every
  one carrying the correct peer PID (`w7 ... recvmsg SCM_CRED ... credpid=56010`), and the
  peer-cred commit `c1c4e79b` proved bidirectional node identity is correct throughout.

- **Yet node-connect still does not COMPLETE.** In the same post-fix run the child logs
  `ChildThreadImpl::EnsureConnected()` (`child_thread_impl.cc:957`) repeatedly — the
  channel-connect watchdog — i.e. the legacy IPC channel never reaches
  `OnChannelConnected(peer_pid)` even though data flows. In Chromium 124 this watchdog is a
  non-fatal `VLOG(0)` (the child keeps running/receiving), which is why the two
  observations (dormant vs. actively-receiving) are *different builds*, not a contradiction.

**Refined root cause statement:** the failure is now **Mojo node-connect completion** —
after the socket, SCM_RIGHTS handle passing, and SCM_CREDENTIALS are all correct, the
renderer's primordial IPC channel never flips to "connected." Everything below that
(transport, bootstrap fd hand-off, peer identity) is exonerated by passing gates.

## 3. What is exonerated, with the reason it's airtight

- **Cross-process fd transport** — gates `xproc-inbound / zygote-inbound / xproc-prearm /
  scm-futex / scm-eventfd` (branch `render/mojo-delivery`).
- **Launch-time bootstrap fd hand-off** — gate `bootstrap-handle` (branch `render/mojo-bootstrap`):
  memfd + SEQPACKET channel survive fork + in-place execve, recovered from argv.
- **Peer credentials / node identity** — `c1c4e79b`: 38+ node messages, correct
  `SCM_CREDENTIALS` both directions.
- **SCM_RIGHTS handle↔data association** — dd does a **single** `recvmsg()` reading data +
  control together into a host scratch buffer (`net.c:1283`), so the macOS kernel
  associates passed fds with the correct stream position; dd only reframes the cmsg header
  (`cmsg_m2l`, `netns.c:404`). No split-read mis-association is possible.
- **Handle-bearing message integrity** — `cmsg_l2m`/`cmsg_import_eventfd_trailer`
  (`netns.c:253-399`) never partially drops SCM_RIGHTS; eventfd wrapping is symmetric and
  the trailer heuristic (WRONLY+NONBLOCK FIFO + magic-marker file) cannot false-positive on
  a memfd.

## 4. The one decisive experiment that was never run

Every prior "mojo" run used either `--v=1` alone (too low — `node_channel`/`node_controller`
log at VLOG(2)) or `--vmodule=*node_channel*=3` **with `--log-level=2`**, which suppresses
VLOG output entirely. **No run has ever captured the actual Mojo node-connect handshake
messages.** Grep confirms: zero occurrences of `AcceptInvitee / AcceptBrokerClient /
AddBrokerClient / MergePorts / SetPeerPid` across all captured logs.

Run this (serialized, idle Mac), then read the *renderer's* stderr:

```
CHROME_EXTRA_FLAGS='--log-level=0 --v=1 \
  --vmodule=node_controller=3,node_channel=3,channel*=3,channel_mojo=3,\
child_thread_impl=3,connection*=3,invitation*=3,ports*=2'
```

The first handshake verb the renderer logs but never completes (expected order:
`AcceptInvitee → AcceptInvitation → AcceptBrokerClient → merge/SetPeerPid`) names the exact
broken step. That single line converts this from "somewhere in node-connect" to a file:line.

## 5. Ranked hypotheses for the completion failure (each with a micro-gate design)

1. **A node-channel control message that carries an out-of-band platform handle
   (`AcceptBrokerClient`/`AcceptInvitee`) is delivered but its attached fd lands as an fd
   the child can't use as a live channel** (e.g. a received socket end whose readiness is
   never re-armed on the child's kqueue). The dormant sample fits: the epoll pumps hold but
   never wake. *Micro-gate:* parent passes a **connected SOCK_STREAM end via SCM_RIGHTS** to
   a fork+execve child that then does `epoll_ctl(ADD, EPOLLIN)` on the *received* fd and
   blocks in `epoll_wait`; parent writes after the child is parked. Existing gates pass a
   *memfd* (scm-futex) or an *inherited* socket (zygote-inbound) — none arms epoll on an
   **SCM_RIGHTS-received socket** and asserts the write wakes it. This is the untested
   permutation closest to `AcceptBrokerClient`.

2. **Message loss/reorder under startup burst on a DGRAM-backed SEQPACKET channel.** The
   network service *recovers on retry* (`mojo-diag.log:84` "crashed, restarting" → the
   restart issues real URL requests), which is the signature of a **timing race**, not a
   hard functional gap. macOS AF_UNIX has no SEQPACKET, so dd backs it with SOCK_DGRAM
   (`net.c:257-324`) on the assumption it is "reliable, ordered." Under a launch-time burst a
   full receive buffer can silently drop a datagram. *Micro-gate:* SEQPACKET socketpair,
   sender bursts N messages larger than the historic 2KB wall while the receiver is briefly
   not draining; assert zero drops/reorders at the guest boundary. (The renderer's *own*
   node channel is STREAM (ty=1) in the logs, so this bites the broker/zygote SEQPACKET
   channels, not the renderer's primary — but a lost broker introduction still wedges the
   renderer.)

3. **macOS SCM fd-count limit when dd triples eventfd fds.** `cmsg_l2m` expands each passed
   eventfd to 3 fds (visible + write-side + marker). A message near Linux's `SCM_MAX_FD`
   (253) with many eventfds can exceed macOS's per-message ancillary limit → the whole
   `sendmsg` fails and the handle-bearing message never arrives. *Micro-gate:* pass K
   emulated eventfds in one SCM_RIGHTS and assert delivery for K up to the point 3K crosses
   the macOS cap.

## 6. Recommended path

- **Interim (documented):** `--single-process` renders content end-to-end (proven). Keep it
  as the supported content path.
- **Structural unblock (in progress on a sibling branch, `agent-ae19d07…`):** switch Chrome
  content to **`wl_shm` CPU raster** (`0b8561bc present wl_shm XRGB content opaque`). With GPU
  compositing off and `zwp_linux_dmabuf` unadvertised, the renderer paints into browser-shared
  memory and there is **no renderer→GPU command-buffer channel to establish** — which very
  likely dissolves Wall 7 outright (see `docs/rendering/ARCHITECTURE-PLAN.md` P1.1). This is
  the same route WSLg and waypipe-on-macOS take and is lower-risk than cracking the Mojo
  node-connect completion.
- **If the Mojo crack is still wanted:** run §4 first to name the step, then implement the
  §5.1 micro-gate; that is the shortest path to a named, reproducible emulation gap.
