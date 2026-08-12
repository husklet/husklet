# Rust execution reachability

`rust-execution` is the temporary, explicit boundary around the retired Rust
guest-execution entry points. It is deliberately absent from `hl-engine`'s
default features. A default `hl-engine` build therefore selects retained C and
rejects `HL_EXECUTION_BACKEND=rust`.

The remaining opt-in production roots are exact and intentionally small:

- `src/runtime/execution.rs`: the `ProductionMachine::Rust` variant and
  `ProductionFactory::rust` selector arm;
- `src/program.rs`: the legacy `hl-engine`, `hl-aarch64`, and `hl-x86_64`
  frontend, which constructs `RustRuntimeFactory` directly;
- `src/apps/engine/Cargo.toml`: the legacy frontend's explicit feature opt-in;
- `src/apps/testing/Cargo.toml`: the compatibility worker's differential-oracle
  opt-in.

Those roots reach the Rust executor closure under
`src/ffi/linux/execution/**`, its `GuestExecutor` exports in `src/ffi.rs`,
`src/ffi/linux.rs`, and `src/native/mod.rs`, and the generic factory in
`src/runtime/machine.rs`. Remove that closure only after both application
opt-ins above have moved to C.

Do **not** infer that the `hl-runtime`, `hl-execution`, `hl-memory`, `hl-task`,
or loader packages can be removed with that closure. The retained C adapter
currently uses typed CPU snapshots and the runtime syscall-trap/task context,
and the product still uses loader inspection plus container lifecycle services.
Their reachability is independent of `rust-execution` and must be audited after
the C adapter owns equivalent services.

Mechanical checks:

```sh
cargo check -p hl-engine --no-default-features --features c-execution
cargo test -p hl-engine --lib --no-default-features --features c-execution \
  production_build_rejects_retired_rust_backend
cargo check -p hl-engine --all-features
```

The first two commands prove that the production C configuration has no Rust
entry root. The all-features command keeps the temporary compatibility closure
buildable until its two named consumers are migrated.
