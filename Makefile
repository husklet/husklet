# Husklet workspace product.
.PHONY: all design-lint lint-cases clippy fmt fmt-check shims test test-ci test-compiles mac-crates mac-gpu containers app dmg install uninstall clean

TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
NIX_DEV = nix develop . --command

# The macOS-gated crates are excluded from default-members and their real code sits behind features, so
# `cargo test --workspace` never builds the Smithay adapter or the Metal presenter. These enable them.
MAC_FEATURES = smithay-adapter,macos-surface
# The Smithay adapter links libxkbcommon from the dev shell; an absent path is a preflight failure, never a
# reason to build less.
MAC_ENV = [ -n "$$HL_LIBXKBCOMMON" ] || { echo "HL_LIBXKBCOMMON is required by the compositor adapter" >&2; exit 1; }; \
	  export RUSTFLAGS="-L native=$$HL_LIBXKBCOMMON/lib $${RUSTFLAGS:-}" \
	         DYLD_LIBRARY_PATH="$$HL_LIBXKBCOMMON/lib:$${DYLD_LIBRARY_PATH:-}";

all: design-lint test

design-lint:
	cargo run -q -p hl-design-lint -- src

lint-cases:
	cargo run -q -p hl-design-lint -- --cases lint src

# `cargo clippy` and `cargo fmt` are toolchain COMPONENTS. Where they are missing — a Linux workspace, for
# which the flake provides no devShell — cargo answers "no such command" and a caller grepping the output
# for warnings sees none and calls it clean. `tools/rust-tool.sh` finds a component matching the active
# rustc and REFUSES loudly when there is none, so a check that could not run cannot read as one that passed.
clippy:
	bash tools/rust-tool.sh clippy --workspace --all-targets

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

# The guest driver shims each declare their own `[workspace]`, so no root cargo command reaches them and a
# failure there stays invisible. Formatting is checked too: the root `fmt --check` cannot see these either.
SHIMS = src/surface/hl-gl/shim/egl \
        src/surface/hl-vulkan/shim/vulkan \
        src/surface/hl-cuda/shim/cuda \
        src/surface/hl-cuda/shim/cudart \
        src/surface/hl-cuda/shim/nvml

shims:
	@for shim in $(SHIMS); do \
	  echo "== $$shim"; \
	  cargo fmt --manifest-path "$$shim/Cargo.toml" -- --check || exit 1; \
	  cargo test --manifest-path "$$shim/Cargo.toml" --no-fail-fast || exit 1; \
	done


test: design-lint
	cargo test -p hl-design-lint -p hl-log -p hl-ws -p hl-ws-term -p hl-gpu

test-ci: fmt-check test shims

containers:
	cargo test -p hl-images -p hl-container -p hl-daemon -p hl-client

# Every test target in the workspace must compile, including the ones no test job runs. A test that stopped
# compiling has been providing zero protection.
test-compiles:
	@[ "$$(uname)" = "Darwin" ] || { echo "test-compiles: requires macOS; the macOS-gated crates cannot be compiled on $$(uname)" >&2; exit 1; }
	$(NIX_DEV) bash -euc '$(MAC_ENV) \
	  cargo test --no-run --workspace --all-targets; \
	  cargo test --no-run -p hl-compositor --features $(MAC_FEATURES) --all-targets'

# The macOS-gated crates with their real features on: the Smithay Wayland adapter and the Metal presenter.
# Device-free — every presenter here is the PngPresenter, so this belongs on a hosted runner.
mac-crates:
	@[ "$$(uname)" = "Darwin" ] || { echo "mac-crates: requires macOS, got $$(uname)" >&2; exit 1; }
	$(NIX_DEV) bash -euc '$(MAC_ENV) \
	  cargo test -p hl-compositor --features $(MAC_FEATURES)'

# The tests that bind a real GPU: the wgpu/Metal host executor and the Metal presenter readback. These need
# a genuine Metal device, so they belong on the self-hosted host.
mac-gpu:
	@[ "$$(uname)" = "Darwin" ] || { echo "mac-gpu: requires macOS, got $$(uname)" >&2; exit 1; }
	@system_profiler SPDisplaysDataType | grep -q "Metal Support" || { echo "mac-gpu: no Metal-capable device on this host" >&2; exit 1; }
	$(NIX_DEV) bash -euc '$(MAC_ENV) \
	  cargo test -p hl-gpu-wgpu; \
	  cargo test -p hl-compositor --features $(MAC_FEATURES) --test macos_present_smoke'

app:
	@chmod +x src/apps/husklet/package/bundle.sh src/apps/husklet/package/make-dmg.sh
	HL_VERSION=$(VERSION) $(NIX_DEV) bash -euc '\
	  export CARGO_TARGET_DIR="$(CURDIR)/target-macos" \
	         HL_BUNDLE_TARGET="$(CURDIR)/target"; \
	  src/apps/husklet/package/bundle.sh $(VERSION)'

dmg: app
	$(NIX_DEV) src/apps/husklet/package/make-dmg.sh $(VERSION)

install: app
	rm -rf /Applications/Husklet.app && cp -R target/Husklet.app /Applications/

uninstall:
	rm -rf /Applications/Husklet.app

clean:
	cargo clean
