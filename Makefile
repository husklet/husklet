# Husklet workspace product.
.PHONY: all design-lint lint-cases clippy fmt fmt-check test test-ci test-compiles containers app dmg install uninstall clean

TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
NIX_DEV = nix develop . --command

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

test: design-lint
	cargo test -p hl-design-lint -p hl-log -p hl-ws -p hl-ws-term

test-ci: fmt-check test

containers:
	cargo test -p hl-images -p hl-container -p hl-daemon -p hl-client

test-compiles:
	cargo test --no-run --workspace --all-targets

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
