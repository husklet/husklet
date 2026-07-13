# Phase 3 — rebrand `dd` to `husklet`

Status: refreshed planning index; no rename is authorized or executed.

The detailed inventories are carried in [`research/`](research/), with the familiar
[`docs/rebrand/`](../../rebrand/README.md) entry point retained as a redirect/index. They were gathered on
2026-07-07 and are useful evidence, but the repository has since added compositor, Rust shim, WSI,
capability, packaging and test surfaces. See the
[`current-tree refresh`](research/current-surface-refresh.md), then re-run every inventory query immediately
before execution and treat the current tree—not recorded counts or line numbers—as authoritative.

Remaining choices and proposed defaults are tracked in [`decisions.md`](decisions.md).
The execution-grade surface classification is in [`contract-map.md`](contract-map.md), the ordered
lockstep plan is in [`atomic-waves.md`](atomic-waves.md), and intentional leftovers are governed by
[`exclusions-and-residue.md`](exclusions-and-residue.md).
Supported launch and diagnostic variables are mapped semantically in
[`environment-contracts.md`](environment-contracts.md); this prevents blind `DD_*` → `HL_*` collision.
Durable state, archives, xattrs, caches, launchd and external identifiers are covered by
[`persisted-data-plan.md`](persisted-data-plan.md).

## Proposed naming contract

| Surface | Planned form |
|---|---|
| product/brand | `husklet` |
| Cargo packages/directories | `husklet-*` |
| Rust crate identifiers | `husklet_*` |
| internal short C/Rust prefix | `hl_` / `HL_` |
| environment variables | `HL_*` |
| state root | `~/.husklet` |
| bundle/service reverse-DNS namespace | `com.husklet.*` |
| Docker-compatible socket basename | keep `docker.sock` |

Binary names remain a release decision. The least surprising proposal is `husklet` for the user CLI and
`husklet-daemon`, `husklet-display`, `husklet-compositor`, `husklet-app`, and `husklet-term` for internal or
secondary executables. Decide this before editing Cargo manifests or packaging.

## Newly required inventory since the original research

- `dd-compositor`, `dd-gpu-wgpu`, and all five `dd-shim-*` crates and their package/lib/export names;
- generated GL/Vulkan/CUDA/CUDART manifests, build scripts, ICD JSON, ELF sonames and loader paths;
- dmabuf generation/device/modifier contracts and `DD_DMABUF_MOD_MAGIC` mirrored across Rust/C;
- capability-handshake and shared GPU IR wire identifiers;
- Smithay runtime selector, compositor binary/socket names and modern protocol fixtures;
- new crate-local test destinations from phase 2;
- current bundle, signing, notarization, release workflow, website media and app identifiers;
- checked-in guest binaries, golden artifacts and manifests whose paths embed `dd`.

## Atomic execution groups

1. **Workspace names:** directory, Cargo package/lib/path dependencies, Make/Nix/package commands and
   phase-2 CI test targets. One commit; workspace metadata must resolve at its end.
2. **Rust/C FFI:** spawn/config structs, exported symbols, headers, layout assertions and consumers.
3. **Cross-process environment:** every setter and reader for daemon, JIT, GPU, display and guest shims.
4. **GPU/display wire:** IR/socket/service names, dmabuf magic, IOSurface and Vulkan ICD contracts.
5. **Persisted paths:** state root, images, workspaces, cache, aliases, xattrs and launchd identifiers.
6. **User surface:** binaries, CLI help, Docker info strings, app bundle, website and release artifacts.

Do not mechanically collapse semantically different variables into the same `HL_*` name. Resolve the
known `DD_VOLUMES` and sandbox collisions first. Cosmetic wire-magic changes require simultaneous producer,
consumer and fixture updates; otherwise keep the numeric value and rename only the constant.

## Phase entry and exit gates

Entry: phase 2 complete, all crate-owned test commands documented, clean current-tree inventory generated,
binary names/collision policy/persisted-data compatibility decided.

Exit: no project-owned `dd` brand remains except an explicit compatibility list; Cargo metadata, all
crate-owned tests, all JIT lanes, daemon real-image quick tests, shim ABI clients, compositor/wgpu mac gate,
packaged app smoke, install/uninstall and website link/media validation pass under husklet names. Record old
data behavior explicitly: migrate, reject with guidance, or intentionally fresh-cutover—never silently
fall back to a second state root.
