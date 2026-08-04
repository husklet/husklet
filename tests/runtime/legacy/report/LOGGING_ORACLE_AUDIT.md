# Logging configuration oracle audit

Studied the retained read-only implementation in `../engine/src/core/log.c`
(`hl_log_parse`, `hl_log_context_init`, `hl_log_enabled`, `hl_log_message`, and
`hl_log_format`), `../engine/src/core/environment.c`
(`hl_environment_debug_log`), `../engine/src/core/options.c`
(`hl_options_import_environment`), and the call sites in
`../engine/src/translator/cache.c` and `../engine/src/linux_abi/syscall/dispatch.c`.

The C context owns a copied tag mask and a borrowed host-service table whose
owner outlives the context. Initialization clears the context before validating
the host logging capability. Logging has no lock or teardown transition: the
immutable context gates formatting before it calls the host emitter. Messages
are bounded to fixed stack buffers and truncation is accepted. Compile-time
logging removal is the only architecture/host branch. Environment capture is a
composition concern: `HL_LOG` is copied into an instance-owned option store and
then parsed when the translator context is initialized. Unknown selector names
are tolerated and open no bits. The syscall tag gates a diagnostic after syscall
dispatch; it does not alter syscall ordering, partial results, cancellation,
signals, or errno.

Rust ownership maps the atomic process-wide tag/level gate to
`src/packages/hl-log/src/state.rs`, parsing to `tag.rs` and `environment.rs`, and
bounded emission/sink dispatch to `emit.rs` and `sink.rs`. Process composition
roots explicitly capture ambient variables and apply the parsed configuration.
Unlike C, Rust retains unknown names as observable startup warnings while still
accepting the usable known subset. Per-engine option transport remains owned by
`src/containers/hl-engine/src/environment.rs` and `options.rs`; compatibility
workers must still pass engine options through `HL_COMPAT_ENGINE_OPTIONS`.

Remaining gap: the Rust engine has not yet reproduced every retained C
translator/syscall diagnostic tag and call site. This configuration port only
opens existing `hl-log` instrumentation and deliberately does not add hot-path
per-syscall logging.
