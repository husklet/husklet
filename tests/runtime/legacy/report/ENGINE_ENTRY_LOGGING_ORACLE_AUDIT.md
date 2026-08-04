# Engine entry logging oracle audit

This lane studied the retained standalone entry in
`../engine/src/core/target/aarch64.c` (`main`, `hl_engine_entry`, and
`hl_standalone_run`), its x86-64 counterpart in
`../engine/src/core/target/x86_64.c`, the logging initialization path in
`../engine/src/translator/cache.c` (`hl_jit_init`), and first-failure handling
in `../engine/src/core/fatal.c` (`hl_fatal_context_init`, `hl_fatal_report`, and
`hl_fatal_status`).

The standalone `main` is deliberately a thin call into the same engine entry
used by library launch. Option state is reset for each entry and `HL_LOG` is
copied into instance-owned options before translator initialization. The
translator owns its log and fatal contexts for the process lifetime; the host
service table outlives them. Fatal publication is first-writer-wins through an
atomic compare/exchange, and later dispatch observes it with acquire ordering.
No teardown or host-specific logging branch exists beyond compile-time logging
support. Logging does not change launch, signal, partial-result, cancellation,
or errno behavior.

Rust maps the two architecture entry binaries to
`src/containers/hl-engine/src/bin/aarch64.rs` and `x86.rs`, while
`Program::run_authorized` owns common launch behavior and error status mapping.
Both entry roots now explicitly capture process logging variables, report
non-fatal parse warnings, and publish structured starting, failed, and exited
events without logging arguments or individual syscalls. Engine launch options
remain separate: compatibility configuration must still use the typed
`HL_COMPAT_ENGINE_OPTIONS` channel.
