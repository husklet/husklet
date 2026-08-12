# Husklet workspace product.
.PHONY: all check design-lint gate gate-app gate-fixture lint lint-c lint-c-inner lint-cases clippy fmt fmt-c fmt-c-inner fmt-check fmt-c-check fmt-c-check-inner test test-ci test-compiles containers engine app dmg install uninstall clean bench-guest bench-gate bench-gate-update bench-gate-arm64 bench-gate-amd64 bench-workloads


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

lint: clippy lint-c

C_LINT_BUILD = target/c-lint-native
C_LINT_CONFIGURE = cmake -S src/runtime/native -B $(C_LINT_BUILD) -DHL_BUILD_TESTS=ON -DCMAKE_C_COMPILER="$(NATIVE_CC)"

lint-c:
	$(NIX_DEV) $(MAKE) lint-c-inner

lint-c-inner:
	$(C_LINT_CONFIGURE)
	cmake --build $(C_LINT_BUILD)
	cmake --build $(C_LINT_BUILD) --target source-manifest-check
	ctest --test-dir $(C_LINT_BUILD) -L lint --no-tests=error --output-on-failure
	cmake --build $(C_LINT_BUILD) --target lint-c

# Rustfmt ships with the pinned flake toolchain, not with a distribution Rust, so route formatting through
# the same shell as Clippy rather than whatever `cargo fmt` a host happens to resolve.
fmt:
	$(NIX_DEV) cargo fmt --all

fmt-c:
	$(NIX_DEV) $(MAKE) fmt-c-inner

fmt-c-inner:
	$(C_LINT_CONFIGURE)
	cmake --build $(C_LINT_BUILD) --target fmt-c

fmt-check:
	$(NIX_DEV) cargo fmt --all -- --check

fmt-c-check:
	$(NIX_DEV) $(MAKE) fmt-c-check-inner

fmt-c-check-inner:
	$(C_LINT_CONFIGURE)
	cmake --build $(C_LINT_BUILD) --target fmt-c-check

test: design-lint
	cargo build -p engine -p testing --bins --locked
	HL_TEST_ENGINE_APP_BIN_DIR="$(CURDIR)/target/debug" cargo test --workspace --all-targets --locked

# The single headless gate: the architecture lint, clippy and every test, run in the dev shell that supplies
# Clippy, pkg-config, GTK4 and the Alpine fixture a bare host toolchain lacks. husklet's `runtime` and
# `gui` features are both off by default and `required-features` makes cargo skip the application
# binary in silence, so each gets its own step or the ~10,800 lines of the shipped app would never be
# compiled by anything but macOS CI.
# Every step runs even after one fails, so a single invocation reports the whole state of the tree.
gate:
	$(NIX_DEV) bash -uc '\
	  status=0; \
	  cargo run -q -p hl-design-lint -- src tests || status=1; \
	  cargo run -q -p hl-design-lint -- --cases lint src tests || status=1; \
	  $(MAKE) lint-c-inner || status=1; \
	  cargo build -p engine -p testing --bins --locked --offline || status=1; \
	  export HL_TEST_ENGINE_APP_BIN_DIR="$(CURDIR)/target/debug"; \
	  cargo clippy --workspace --all-targets --locked --offline -- -D warnings || status=1; \
	  cargo clippy -p husklet --all-targets --features runtime --locked --offline -- -D warnings || status=1; \
	  cargo clippy -p husklet --all-targets --features gui --locked --offline -- -D warnings || status=1; \
	  cargo test --workspace --all-targets --locked --offline --no-fail-fast || status=1; \
	  cargo test --workspace --doc --locked --offline || status=1; \
	  exit $$status'

# The Alpine-fixture tests, which the workspace sweep never runs because they are `#[ignore]`d behind
# HL_ALPINE_ARCHIVE. Deliberately not part of `gate`: they need the fixture and a host `cc` that can
# link `-static`, so they are environment-conditional in a way the sweep is not. Each case runs alone
# under a hard timeout because a stuck guest otherwise hangs the whole invocation. Promote a case into
# FIXTURE_CASES only once it has been proven green and non-vacuous; `descendant_cleanup` stays out
# because it never terminates and its liveness assertions probe host pids for guest processes.
FIXTURE_TIMEOUT ?= 180
FIXTURE_CASES = \
  hl-container:filesystem_coherence:new_file_is_visible \
  hl-container:filesystem_coherence:overwritten_file_is_visible \
  hl-container:filesystem_coherence:directory_tree_is_visible \
  hl-container:filesystem_coherence:held_directory_is_coherent \
  hl-container:lifecycle_contract:hangup_reaches_the_guest_signal_handler \
  hl-container:lifecycle_contract:configured_quit_reaches_the_guest_signal_handler \
  hl-container:lifecycle_contract:pause_stops_guest_progress_until_unpause \
  hl-container:lifecycle_contract:health_probes_reach_healthy_and_unhealthy_states \
  hl-container:process_contract:sigterm_stop \
  hl-container:process_contract:exec_contracts \
  hl-container:run_options:process_run_options \
  hl-daemon:daemon-api:shared_mount_lock_contention

gate-fixture:
	$(NIX_DEV) bash -uc '\
	  export HL_TEST_ENGINE_APP_BIN_DIR="$(CURDIR)/target/debug"; \
	  cargo build -p engine -p testing --bins --locked --offline || exit 1; \
	  cargo test -p hl-container -p hl-daemon --tests --locked --offline --no-run || exit 1; \
	  status=0; \
	  for entry in $(FIXTURE_CASES); do \
	    package=$${entry%%:*}; rest=$${entry#*:}; target=$${rest%%:*}; name=$${rest#*:}; \
	    echo "== $$package $$target $$name"; \
	    timeout -s KILL $(FIXTURE_TIMEOUT) cargo test -p $$package --test $$target --locked --offline \
	      -- --exact --ignored --nocapture $$name || status=1; \
	  done; \
	  exit $$status'

# husklet's `runtime` feature is off by default and `gui` needs the macOS GTK stack, so the workspace
# sweep alone never compiles the container-runtime surface.
check:
	cargo check --workspace --all-targets --locked
	cargo check -p husklet --all-targets --features runtime --locked
	cargo check -p husklet --all-targets --features gui --locked

# What a plain `cargo clippy --workspace --all-targets` cannot reach: `required-features = ["gui"]`
# makes cargo skip the shipped application binary without a word when the feature is off. This runs
# exactly the two feature-gated commands CI runs on macOS, and the Linux dev shell now carries GTK4
# and VTE so both are answerable here.
gate-app:
	$(NIX_DEV) bash -uc '\
	  status=0; \
	  cargo clippy -p husklet --all-targets --features runtime --locked --offline -- -D warnings || status=1; \
	  cargo clippy -p husklet --all-targets --features gui --locked --offline -- -D warnings || status=1; \
	  exit $$status'

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
BENCH_MAX_SPREAD ?= 0.05
BENCH_GATE = target/release/testing benchmark gate \
	  --workload $(BENCH_WORKLOAD) --arch $(BENCH_ARCH) --binary $(BENCH_GUEST) \
	  --c-build $(BENCH_C_BUILD) --rust-engine $(CURDIR)/target/release/hl-engine \
	  --repeats $(BENCH_REPEATS) --divisor $(BENCH_DIVISOR) --max-spread $(BENCH_MAX_SPREAD)

# The guest ISA selects the lowering under test: arm64 covers
# src/runtime/native/exec/src/arch/aarch64, amd64 covers .../x86_64. Prove an x86-64
# change with bench-gate-amd64, never with bench-gate-arm64.
BENCH_CC_arm64 = aarch64-linux-gnu-gcc
BENCH_CC_amd64 = x86_64-linux-gnu-gcc
BENCH_CC = $(BENCH_CC_$(BENCH_ARCH))

bench-guest:
	@mkdir -p $(dir $(BENCH_GUEST))
	@command -v $(BENCH_CC) >/dev/null || { echo "install $(BENCH_CC) to build the $(BENCH_ARCH) guest"; exit 1; }
	$(BENCH_CC) -O2 -static -o $(BENCH_GUEST) tests/bench/combined/main.c

bench-gate: bench-guest
	cargo build --release -p engine -p testing --bins --locked
	$(BENCH_GATE)

bench-gate-update: bench-guest
	cargo build --release -p engine -p testing --bins --locked
	$(BENCH_GATE) --update

bench-gate-arm64:
	$(MAKE) bench-gate BENCH_ARCH=arm64

bench-gate-amd64:
	$(MAKE) bench-gate BENCH_ARCH=amd64

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
