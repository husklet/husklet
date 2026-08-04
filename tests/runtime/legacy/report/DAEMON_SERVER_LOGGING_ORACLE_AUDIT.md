# Daemon server logging oracle audit

The retained engine has no Docker HTTP daemon, so the nearest read-only server
oracle is `../engine/src/linux_abi/fork.c` (`hl_server_main`, `srv_sigint`, and
`hl_forkserver_runner`). The lane also studied `../engine/src/core/log.c`
(`hl_log_context_init`, `hl_log_enabled`, and `hl_log_message`).

The C server owns one listening Unix socket, a fixed table of live child/process
connections, and a child-watch descriptor. Signal handlers only set a
`sig_atomic_t` stop flag and wake the ordinary poll loop. The loop accepts after
readiness, tolerates `EINTR`, closes each per-request descriptor on every
rejection path, reaps children without blocking, then closes the listener and
unlinks its socket during teardown. Startup failures report before returning;
normal listen state is reported once. Host and guest ISA branches are outside
the server lifecycle. Server diagnostics do not alter request results, partial
I/O, cancellation, signals, or errno.

Rust ownership maps the Docker HTTP listener, connection join set, shutdown
publication, and socket identity guard to
`src/containers/hl-daemon/src/server.rs`. The application root owns signal
selection. The server now publishes one human and structured diagnostic at each
coarse lifecycle boundary: starting, listening, graceful stopping, stopped, and
fatal failure. It deliberately emits nothing per accepted connection or HTTP
request. `SocketGuard` continues to remove the path only when its device/inode
identity proves ownership.
