# Husklet

Husklet provides configurable Linux workspaces on macOS. A workspace combines an OCI image,
terminal, mounts, environment, networking, optional container services, and optional GUI/GPU
capabilities in one signed desktop application.

## Architecture

The `husklet` application is the composition root. It translates workspace configuration into the
typed container API from `src/containers`; `hl-container` lowers each launch into the Rust engine API
from `../engine`. Product code does not invoke an engine binary or construct an engine-specific
environment dialect.

Graphics follows the same composition model:

```text
workspace configuration
  -> husklet Graphics device
  -> hl-container Device contract
  -> hl-engine MachineSpec extensions and mounts
  -> guest GL / CUDA / Vulkan shims
  -> hl-gpu neutral IR
  -> hl-gpu-wgpu host execution
  -> hl-compositor
  -> native macOS surface
```

Read-only guest libraries are projected through the engine's versioned `engine.namespace`
extension. Live GPU and Wayland sockets remain typed writable container mounts until the engine
supports writable socket projection. Backend selection and service lifetime stay in Husklet;
container and engine crates remain GPU-neutral.

Repository crates are grouped under `src/`:

```text
src/
  apps/husklet/          signed product and composition root
  gpu/                   neutral GPU protocol and host backend
  surface/               compositor and guest GL/CUDA/Vulkan libraries
  workspaces/            workspace models, terminal, and generic GUI
  packages/              reusable foundations
```

The container and engine repositories are sibling dependencies:

```text
src/containers/          images, container lifecycle, daemon, client
../engine/               typed Linux execution engine
```

## Development

Portable checks:

```sh
make test
cargo check -p husklet --features runtime --lib
cargo test -p hl-container
```

The macOS GUI requires the pinned Nix development shell:

```sh
nix develop "path:$PWD/nix" --command \
  cargo check -p husklet --bin husklet --features gui
```

Build the signed application bundle with:

```sh
make app
```

See [AGENTS.md](AGENTS.md) for stable architecture and design rules, and
[docs/ENGINE.md](docs/ENGINE.md) for the engine capabilities required to remove current runtime
workarounds.
