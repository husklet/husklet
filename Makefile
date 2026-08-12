# Husklet workspace product.
.PHONY: all check design-lint gate gate-app gate-compat gate-fixture lint lint-c lint-c-inner lint-cases clippy fmt fmt-c fmt-c-inner fmt-check fmt-c-check fmt-c-check-inner test test-ci test-compiles containers engine app dmg install uninstall clean bench-product-ab-prepare bench-product-ab bench-direct-ab bench-workloads


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
	cmake --build $(C_LINT_BUILD) --target strict-warnings-c
	cargo run -q -p hl-design-lint -- --native $(C_LINT_BUILD) src/runtime/native

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
	  src/runtime/native/exec/test/memory_lifecycle.sh || status=1; \
	  exit $$status'

# The full runtime compatibility sweep is intentionally separate from `gate`: it executes thousands
# of guest cases and is evidence for a release-sized engine, not a minute-scale PR check. Every run
# gets a fresh directory and a nonexistent ledger path, so the resumable runner cannot replay an old
# result set. The directory is retained after the run for diagnosis and comparison.
COMPAT_JOBS ?= 4
gate-compat:
	$(NIX_DEV) bash -euc '\
	  cargo build --release -p engine -p testing --bins --locked --offline; \
	  export HL_TEST_ENGINE_APP_BIN_DIR="$(CURDIR)/target/release"; \
	  export HL_COMPAT_ARM64_CC=/usr/bin/aarch64-linux-gnu-gcc; \
	  export HL_COMPAT_AMD64_CC=/usr/bin/x86_64-linux-gnu-gcc; \
	  test -x "$$HL_COMPAT_ARM64_CC"; \
	  test -x "$$HL_COMPAT_AMD64_CC"; \
	  for worker in hl-aarch64 hl-x86_64; do \
	    "$$HL_TEST_ENGINE_APP_BIN_DIR/$$worker" --backend-receipt \
	      | grep -F '\''"backend":"retained-c"'\'' >/dev/null; \
	  done; \
	  mkdir -p "$(CURDIR)/target/testing/runtime"; \
	  run_dir="$$(mktemp -d "$(CURDIR)/target/testing/runtime/gate.XXXXXX")"; \
	  results="$$run_dir/results.tsv"; \
	  test ! -e "$$results"; \
	  echo "runtime compatibility results: $$results"; \
	  target/release/testing runtime --jobs "$(COMPAT_JOBS)" \
	    --results "$${results#$(CURDIR)/}" --baseline tests/runtime/baseline.tsv \
	    --engine-profile release'

# The Alpine-fixture tests, which the workspace sweep never runs because they are `#[ignore]`d behind
# HL_ALPINE_ARCHIVE. Deliberately not part of `gate`: they need the fixture and a host `cc` that can
# link `-static`, so they are environment-conditional in a way the sweep is not. Each case runs alone
# under a hard timeout because a stuck guest otherwise hangs the whole invocation. Promote a case into
# FIXTURE_CASES only once it has been proven green and non-vacuous.
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
  hl-daemon:daemon-api:descendant_cleanup \
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
	$(NIX_DEV) cargo build --release -p engine --bins --locked --offline

# Authoritative product-boundary C-vs-C campaign. Preparation copies completed
# workers in separate phases, hashes and smokes them before use; execution
# alternates explicit-C/default-C order and refuses reused artifact/result paths.
PRODUCT_AB_ISA ?= arm64
PRODUCT_AB_BENCHMARK ?= lifecycle
PRODUCT_AB_CASE ?= lifecycle
PRODUCT_AB_ROUNDS ?= 6
PRODUCT_AB_RUN ?=
PRODUCT_AB_ARTIFACTS = target/testing/product-ab/artifacts/$(PRODUCT_AB_RUN)
PRODUCT_AB_RESULTS = target/testing/product-ab/results/$(PRODUCT_AB_RUN).tsv

bench-product-ab-prepare:
	@test -n "$(PRODUCT_AB_RUN)" || { echo "set PRODUCT_AB_RUN to a new campaign id"; exit 1; }
	cargo build --release -p testing --bin testing --locked
	target/release/testing product-ab-prepare --isa $(PRODUCT_AB_ISA) --artifacts $(PRODUCT_AB_ARTIFACTS)

bench-product-ab:
	@test -n "$(PRODUCT_AB_RUN)" || { echo "set PRODUCT_AB_RUN to the prepared campaign id"; exit 1; }
	target/release/testing product-ab $(PRODUCT_AB_BENCHMARK) $(PRODUCT_AB_CASE) \
	  --isa $(PRODUCT_AB_ISA) --rounds $(PRODUCT_AB_ROUNDS) \
	  --artifacts $(PRODUCT_AB_ARTIFACTS) --results $(PRODUCT_AB_RESULTS)

# Direct preserved-artifact comparison used by the final ERI C-vs-C campaign.
# Establish a same-binary null result first, then provide it when BASE and
# CANDIDATE differ. Paths are deliberately required so Make cannot select an
# implicit sibling checkout or reuse a ledger.
AB_BASE ?=
AB_CANDIDATE ?=
AB_GUEST ?=
AB_RESULTS ?=
AB_NULL_RESULTS ?=
AB_ROUNDS ?= 6

bench-direct-ab:
	@test -n "$(AB_BASE)" -a -n "$(AB_GUEST)" -a -n "$(AB_RESULTS)" || \
	  { echo "set AB_BASE, AB_GUEST, and a new AB_RESULTS path"; exit 1; }
	target/release/testing ab --base $(AB_BASE) $(if $(AB_CANDIDATE),--candidate $(AB_CANDIDATE)) \
	  --guest $(AB_GUEST) --rounds $(AB_ROUNDS) --results $(AB_RESULTS) \
	  $(if $(AB_NULL_RESULTS),--null-arm-results $(AB_NULL_RESULTS))

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
