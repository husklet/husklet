# Husklet workspace product.
.PHONY: all check design-lint lint-cases clippy fmt fmt-check test test-ci test-compiles containers engine app dmg install uninstall clean bench-guest bench-gate bench-gate-update bench-workloads

TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
NIX = nix --extra-experimental-features 'nix-command flakes'
NIX_DEV = $(NIX) develop . --command

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

test-ci:
	$(NIX) flake check -L --option cores 0 --max-jobs auto

containers:
	cargo test -p hl-images -p hl-container -p hl-daemon -p hl-client -p dockerd

test-compiles:
	cargo test --no-run --workspace --all-targets

engine:
	cargo build --release -p engine --bins --locked

# Authoritative Rust-vs-retained-C verdict. The harness selects the retained engine and its
# exec wrapper from BENCH_C_BUILD, pins one CPU, and refuses a verdict it cannot trust.
BENCH_WORKLOAD ?= compute
BENCH_ARCH ?= arm64
BENCH_C_BUILD ?= $(CURDIR)/../engine/build/unit-audit
BENCH_GUEST ?= $(CURDIR)/target/testing/bench/combined/$(BENCH_ARCH)/combined-bench
BENCH_REPEATS ?= 7
BENCH_DIVISOR ?= 1
BENCH_GATE = target/release/testing benchmark gate \
	  --workload $(BENCH_WORKLOAD) --arch $(BENCH_ARCH) --binary $(BENCH_GUEST) \
	  --c-build $(BENCH_C_BUILD) --rust-engine $(CURDIR)/target/release/hl-engine \
	  --repeats $(BENCH_REPEATS) --divisor $(BENCH_DIVISOR)

bench-guest:
	@mkdir -p $(dir $(BENCH_GUEST))
	cc -O2 -static -o $(BENCH_GUEST) tests/bench/combined/main.c

bench-gate: bench-guest
	cargo build --release -p engine -p testing --bins --locked
	$(BENCH_GATE)

bench-gate-update: bench-guest
	cargo build --release -p engine -p testing --bins --locked
	$(BENCH_GATE) --update

bench-workloads:
	cargo run -q --release -p testing --bin testing -- benchmark workloads

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
