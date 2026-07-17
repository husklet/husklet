# dd workspace.
.PHONY: all jit fmt fmt-check test test-ci mac-crates perf test-docker test-docker-full test-compose test-docker-net test-realsw test-smoke scenarios scenarios-real scenarios-long scenarios-count scenarios-clean coverage clean app dmg install uninstall
# Version is the git tag (v0.2.0 -> 0.2.0); falls back to 0.0.0-dev with no tags. CI passes it too.
TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
# Run a command inside the GTK4 dev shell (provides pkg-config/gtk4 + packaging tools on macOS).
NIX_DEV = nix develop "path:$(CURDIR)/nix" --command
all: jit
jit:            ## build + codesign both guest-arch JITs (via cargo build.rs) + the crates
	cargo build --release
test: jit       ## run the engine × case matrix (grouped report); FILTER=name ENGINE=x86_64 to narrow
	cargo run -q -p hl-jit-darwin --example matrix -- $(if $(ENGINE),-e $(ENGINE)) $(FILTER)
test-ci: jit    ## the cargo-test path (one matrix test; for CI)
	cargo test -p hl-jit-darwin
mac-crates:     ## POST-MERGE GATE (macOS): build+test the mac-only GPU/compositor crates outside default-members.
	@[ "$$(uname)" = "Darwin" ] || { echo "mac-crates: macOS-only (hl-compositor links libxkbcommon + the Cocoa/Metal present path) — skipping on $$(uname). Maintainer: run via the mac bridge."; exit 0; }
	$(NIX_DEV) bash -euc '\
	  X="$$HL_LIBXKBCOMMON"; [ -n "$$X" ] || { echo "mac-crates: HL_LIBXKBCOMMON not exported by the dev shell — update nix/flake.nix"; exit 1; }; \
	  export RUSTFLAGS="-L native=$$X/lib $${RUSTFLAGS:-}" DYLD_LIBRARY_PATH="$$X/lib:$${DYLD_LIBRARY_PATH:-}"; \
	  cargo build -p hl-gpu-wgpu -p hl-compositor; \
	  cargo test  -p hl-compositor -p hl-gpu-wgpu'
perf: jit       ## same matrix + an oracle-vs-JIT slowdown table & summary (PERF_N=median runs; writes target/hl-tests/perf.{csv,json}); FILTER/ENGINE narrow
	PERF=1 cargo run -q -p hl-jit-darwin --example matrix -- $(if $(ENGINE),-e $(ENGINE)) $(FILTER)
test-docker: jit ## end-to-end Docker-CLI scenarios against hl-daemon (run/logs/stop/kill/volumes/networks)
	bash src/containers/hl-daemon/testdata/scenarios/docker.sh
test-docker-full: jit ## FULL Docker CLI/API compliance matrix (every command; maps each failure to a non-compliant verb)
	bash src/containers/hl-daemon/testdata/scenarios/docker-full.sh
test-compose: jit ## end-to-end Docker Compose scenarios against hl-daemon (up/ps/logs/exec/down; skips if no compose)
	bash src/containers/hl-daemon/testdata/scenarios/compose.sh
	bash src/containers/hl-daemon/testdata/scenarios/compose-multinet.sh
test-docker-net: jit ## container-to-container networking (by-name DNS / by-IP / cross-network isolation)
	bash src/containers/hl-daemon/testdata/scenarios/docker-net.sh
test-realsw: jit ## run REAL pulled software (redis/python/postgres/nats) with deterministic workloads
	bash src/containers/hl-daemon/testdata/scenarios/realsw.sh
test-smoke:     ## user-perspective: FRESH-PULL + run a real glibc distro on BOTH arches (the libc.so.6 guard; needs network, macOS)
	cargo build --release -p husklet -p hl-daemon
	bash src/containers/hl-daemon/testdata/scenarios/smoke-realimage.sh
scenarios: jit  ## REAL software through hl-daemon (the SUT): popular images, both arches. CAT=databases TGT=arm to narrow
	cargo test -q -p hl-daemon --test scenarios -- --backend dd $(if $(CAT),-c $(CAT)) $(if $(TGT),-t $(TGT))
scenarios-real: ## same scenarios against the REAL docker oracle (mac Docker Desktop) — proves the tests are correct
	cargo test -q -p hl-daemon --test scenarios -- --backend real --long $(if $(CAT),-c $(CAT)) $(if $(TGT),-t $(TGT))
scenarios-long: jit ## full compatibility sweep against hl-daemon (pulls images, heavy workloads, both arches)
	cargo test -q -p hl-daemon --test scenarios -- --backend dd --long $(if $(CAT),-c $(CAT))
scenarios-count: ## list every scenario×target case + total (runs nothing) — proves the case count
	cargo test -q -p hl-daemon --test scenarios -- --count $(if $(CAT),-c $(CAT))
scenarios-clean: ## reap ONLY harness-spawned hl-daemons (by pidfile, mac-side) + remove scratch — keeps the host clean
	@for pf in target/hl-scen/*/daemon.pid; do [ -f "$$pf" ] || continue; pid=$$(cat "$$pf"); \
	  mac bash -lc "kill $$pid 2>/dev/null; true" </dev/null; echo "reaped $$pid"; done; \
	  rm -rf target/hl-scen/hl-* target/hl-scen/real-* target/hl-scen/*.sh 2>/dev/null; echo "scratch cleaned"
scenarios-prune: ## DISK reclaim: drop UNUSED images from the mac docker ORACLE (Docker Desktop). Removes anything no container uses; re-pulls on next run. Opt-in.
	@echo "oracle disk BEFORE:"; mac docker system df </dev/null 2>&1 | head -2; \
	  mac docker image prune -af </dev/null 2>&1 | tail -1; \
	  echo "oracle disk AFTER:"; mac docker system df </dev/null 2>&1 | head -2
coverage: jit  ## report unimplemented syscalls/opcodes (static switch-diff + dynamic corpus run); MODE=static|dynamic|all
	bash src/engine/hl-jit-darwin/tools/coverage.sh $(or $(MODE),all)
# The decomposed C engine lives under hl-jit-darwin/src/runtime/{engine,translate,host,include,os,targets}
# (os/ covers both os/linux and os/darwin). Uses hl-jit-darwin/.clang-format.
RUNTIME_C = $(shell find src/engine/hl-jit-darwin/src/runtime/engine src/engine/hl-jit-darwin/src/runtime/translate src/engine/hl-jit-darwin/src/runtime/host src/engine/hl-jit-darwin/src/runtime/include src/engine/hl-jit-darwin/src/runtime/os src/engine/hl-jit-darwin/src/runtime/targets \( -name '*.c' -o -name '*.h' \))
fmt:            ## format the whole tree: clang-format the C engine + cargo fmt the Rust crates
	clang-format -i $(RUNTIME_C)
	cargo fmt --all
fmt-check:      ## CI: verify clang-format + cargo fmt are clean (no writes)
	clang-format --dry-run --Werror $(RUNTIME_C)
	cargo fmt --all -- --check
app:            ## build + assemble & ad-hoc-sign build/hl.app (the GTK GUI bundle; macOS)
	@chmod +x src/apps/husklet/package/bundle.sh src/apps/husklet/package/make-dmg.sh
	cargo clean -p hl-jit-darwin --release                 # FORCE a fresh C engine: build.rs's .c rerun-if-changed is unreliable under CI rust-cache, so a stale engine could otherwise ship (Rust/daemon fixes shipped while engine/C fixes silently didn't)
	cargo build --release -p hl-daemon -p husklet
	HL_VERSION=$(VERSION) $(NIX_DEV) src/apps/husklet/package/bundle.sh $(VERSION)
dmg: app        ## build dist/hl-<ver>-<arch>.dmg from the app bundle (macOS)
	$(NIX_DEV) src/apps/husklet/package/make-dmg.sh $(VERSION)
install: app    ## copy the app to /Applications and run `dd install` (per-user, no root)
	rm -rf /Applications/hl.app && cp -R target/hl.app /Applications/
	cargo run -q -p husklet --bin hl -- install
uninstall:      ## remove the daemon agent + docker context (keeps ~/.hl unless --purge)
	cargo run -q -p husklet --bin hl -- uninstall
clean:
	cargo clean
