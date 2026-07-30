# Husklet workspace product.
.PHONY: all design-lint lint-cases fmt fmt-check test test-ci mac-crates containers app dmg install uninstall clean

TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
NIX_DEV = nix develop . --command

all: design-lint test

design-lint:
	cargo run -q -p hl-design-lint -- src

lint-cases:
	cargo run -q -p hl-design-lint -- --cases lint src

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

test: design-lint
	cargo test -p hl-design-lint -p hl-log -p hl-ws -p hl-ws-term -p hl-gpu

test-ci: fmt-check test

containers:
	cargo test -p hl-images -p hl-container -p hl-daemon -p hl-client

mac-crates:
	@[ "$$(uname)" = "Darwin" ] || { echo "mac-crates: macOS-only; skipping on $$(uname)"; exit 0; }
	$(NIX_DEV) bash -euc '\
	  export RUSTFLAGS="-L native=$$HL_LIBXKBCOMMON/lib $${RUSTFLAGS:-}" \
	         DYLD_LIBRARY_PATH="$$HL_LIBXKBCOMMON/lib:$${DYLD_LIBRARY_PATH:-}"; \
	  cargo build -p hl-gpu-wgpu -p hl-compositor; \
	  cargo test -p hl-compositor -p hl-gpu-wgpu'

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
