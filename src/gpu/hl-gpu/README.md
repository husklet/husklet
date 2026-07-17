# hl-gpu (staging)

Staging reimplementation of the GPU core per **`docs/hl_wip-OVERVIEW-v2.md`**. On completion this
replaces `hl-gpu` (drop `_wip`).

Standalone crate: `Cargo.toml` has an empty `[workspace]` table, so it is **excluded from the repo-root
workspace** — the shared tree stays green regardless of its state. Build/test it explicitly:

```
cargo test --manifest-path hl-gpu/Cargo.toml
```

Package `hl_gpu`, lib name `hl_gpu`.

## Modules (v2 §3)

- `protocol/` — **done.** The neutral language + the port drivers submit through. `model/` (id, error,
  enums, descriptor, command, capability, kernel) · `codec/` (wire, encode, decode, tag) · `port/`
  (`CommandSink`). Wire byte-identical to shipping `hl-gpu` (WIRE_VERSION=4; proven by cross-encode
  golden vectors). No cuda/vulkan/gl/platform types. Shader payloads classified by neutral magic
  (`KERNEL_MAGIC`/`SPIRV_MAGIC`) — the old ptx leak is broken.
- `transport/` — pending. framing + socket; `client` (RemoteCommandSink) + `server` serve-loop.
- `runtime/` — pending. per-connection `Session`: validate → account → dispatch; `GpuExecutor` port.
- `cpu/` — pending. reference `GpuExecutor` (must pass the frozen conformance suite; it is the oracle).

Acceptance gate: the frozen goldens in `hl-gpu/tests/{golden,conformance}.rs`.
