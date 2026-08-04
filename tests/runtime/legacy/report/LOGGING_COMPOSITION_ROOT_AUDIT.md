# Logging composition-root audit

## Mechanical inventory

Every workspace `src/main.rs`, manifest-declared `[[bin]]`, and immediate
`src/bin/*.rs` was inspected together with its manifest and callers.

| Executable | Ownership | Environment configuration |
|---|---|---|
| `husklet` | product composition root | `hl::logging::configure` before application construction |
| `hl-daemon` | Docker-compatible server composition root | `EnvironmentConfig::parse` before argument and service work |
| `hl-engine` | integrated engine composition root | `EnvironmentConfig::parse` before launch parsing |
| `hl-aarch64`, `hl-x86_64` | architecture engine entry roots | `EnvironmentConfig::parse` before worker launch |
| `testing` | repository test composition root | parser with explicit `EXEC`/`Error` defaults |
| `hl-syscall-audit`, `hl-design-lint` | repository-only tools | intentionally have no production logging policy |
| `hl-compat-worker`, `hl-native-child-fixture`, `hl-confinement-child`, `hl-projection-worker` | compatibility or confinement fixtures | intentionally preserve their test protocols without ambient logging setup |
| `hl-authority-child` | privilege-separated engine child | supervisor supplies an empty environment, so it has no ambient configuration contract |

This inventory leaves no uncovered production composition root. Adding parser
calls to repository tools or protocol fixtures would create policy outside the
requested production boundary. Adding one to `hl-authority-child` would be inert:
`ProcessAuthority::spawn` constructs its `SpawnRequest` with
`environment: Vec::new()` deliberately, so the child cannot inherit host
configuration or secrets.

The checked tree can reproduce the coverage assertion without executing any
binary:

```sh
for root in \
  src/apps/husklet/src/logging.rs \
  src/apps/testing/src/main.rs \
  src/containers/hl-daemon/src/bin/hl-daemon.rs \
  src/containers/hl-engine/src/main.rs \
  src/containers/hl-engine/src/bin/aarch64.rs \
  src/containers/hl-engine/src/bin/x86.rs
do
  rg -q 'EnvironmentConfig' "$root"
done
rg -q 'environment: Vec::new\(\)' src/containers/hl-engine/src/native/authority.rs
rg -q 'Purpose-built native-launch fixture' src/containers/hl-engine/src/bin/child-fixture.rs
```

This command passed on the audited working tree. Manifest discovery used
`rg --files src -g Cargo.toml`, followed by the declared `[[bin]]` paths; direct
roots were cross-checked with `rg --files src | rg '/src/(main|bin/[^/]+)\.rs$'`.

## Retained C oracle

The audit studied `../engine/src/core/environment.c`
(`hl_environment_debug_log`), `../engine/src/core/options.c`
(`hl_options_import_environment`), `../engine/src/core/log.c`
(`hl_log_context_init` and `hl_log_enabled`),
`../engine/src/translator/cache.c` (translator log-context initialization), and
`../engine/src/linux_abi/sentry.c` (`sentry_init`, `sentry_loop`, and
`sentry_shutdown`).

The retained engine reads `HL_LOG` at its outer launch boundary, copies it into
instance-owned option storage, and initializes the translator's immutable tag
mask from that copy. Its privilege-separated sentry is forked only after engine
and host-service initialization, so it inherits already-established state; it
does not independently reinterpret ambient configuration. The sentry owner
alone publishes shutdown and reaps the child. Its acquire/release quit flag and
first-owner lifetime do not alter syscall results, partial I/O, cancellation,
signals, or errno.

Rust intentionally differs in process mechanics while retaining the boundary:
outer executable roots capture logging variables once, while the separately
spawned authority process receives an explicit empty environment. Compatibility
engine options continue to travel only through typed
`HL_COMPAT_ENGINE_OPTIONS`; ambient `HL_LOG` on a supervisor is not evidence of
a guest-engine mode or option.
