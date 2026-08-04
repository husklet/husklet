# Husklet workspace product.
.PHONY: all check design-lint lint-cases clippy fmt fmt-check test test-ci test-compiles containers engine app dmg install uninstall clean

TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
NIX_DEV = nix --extra-experimental-features 'nix-command flakes' develop . --command

all: test-ci

design-lint:
	cargo run -q -p hl-design-lint -- src tests

lint-cases:
	cargo run -q -p hl-design-lint -- --cases lint src tests

# The flake pins Rust and provides Clippy for every supported development host. Keep dependency resolution
# locked and offline so a missing tool or source is a hard failure rather than an unreviewed environment change.
clippy:
	$(NIX_DEV) cargo clippy --workspace --all-targets --locked --offline -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

test: design-lint
	cargo build -p engine -p testing --bins --locked
	HL_TEST_ENGINE_APP_BIN_DIR="$(CURDIR)/target/debug" cargo test --workspace --all-targets --locked

check:
	cargo check --workspace --all-targets --locked

test-ci: fmt-check design-lint check test

containers:
	cargo test -p hl-images -p hl-container -p hl-daemon -p hl-client -p dockerd

test-compiles:
	cargo test --no-run --workspace --all-targets

engine:
	cargo build --release -p engine --bins --locked

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
