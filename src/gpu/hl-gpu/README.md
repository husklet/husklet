# hl-gpu (staging)

GPU core primitives. On completion this
replaces `hl-gpu` (drop `_wip`).

Workspace crate. Build and test it explicitly:

```
cargo test -p hl-gpu --all-targets
```

Package `hl_gpu`, lib name `hl_gpu`.

## Modules (v2 §3)

- `protocol/` — **done.** The neutral language + the port drivers submit through. `model/` (id, error,
  enums, descriptor, command, capability, kernel) · `codec/` (wire, encode, decode, tag) · `port/`
  (`CommandSink`). Wire byte-identical to shipping `hl-gpu` (WIRE_VERSION=14; proven by cross-encode
  golden vectors). No cuda/vulkan/gl/platform types. Shader payloads classified by neutral magic
  (`KERNEL_MAGIC`/`SPIRV_MAGIC`) — the old ptx leak is broken.
- `transport/` — pending. framing + socket; `client` (RemoteCommandSink) + `server` serve-loop.
  Submit framing stays byte-identical within `WIRE_VERSION`. The capability handshake requires the exact
  wire version, and both peers must come from a build that implements that version's readback request
  kinds; fence poll/wait are additive readback kinds, not a fallback for an older same-version host.
- `runtime/` — pending. per-connection `Session`: validate → account → dispatch; `GpuExecutor` port.
- `cpu/` — pending. reference `GpuExecutor` (must pass the frozen conformance suite; it is the oracle).

Acceptance gate: the frozen goldens in `hl-gpu/tests/{golden,conformance}.rs`.
