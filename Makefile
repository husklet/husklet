# dd workspace.
.PHONY: all jit fmt fmt-check test test-ci mac-crates perf test-docker test-docker-full test-compose test-docker-net test-macos test-realsw test-smoke scenarios scenarios-real scenarios-long scenarios-count scenarios-clean coverage bench clean app dmg install uninstall mac-image mac-push
# Version is the git tag (v0.2.0 -> 0.2.0); falls back to 0.0.0-dev with no tags. CI passes it too.
TAG := $(shell git describe --tags --abbrev=0 2>/dev/null | sed 's/^v//')
VERSION ?= $(or $(TAG),0.0.0-dev)
# Run a command inside the GTK4 dev shell (provides pkg-config/gtk4 + packaging tools on macOS).
NIX_DEV = nix develop "path:$(CURDIR)/nix" --command
all: jit
jit:            ## build + codesign both guest-arch JITs (via cargo build.rs) + the crates
	cargo build --release
test: jit       ## run the engine × case matrix (grouped report); FILTER=name ENGINE=x86_64 to narrow
	cargo run -q -p dd-tests -- $(if $(ENGINE),-e $(ENGINE)) $(FILTER)
test-ci: jit    ## the cargo-test path (one matrix test; for CI)
	cargo test -p dd-tests
mac-crates:     ## POST-MERGE GATE (macOS): build+test the mac-only Wayland-renderer crates (dd-display/dd-gpu-wgpu/dd-compositor). They are NOT in workspace default-members, so a plain `cargo build` never compiles them — run this after any merge that touches shared types they use (present.rs SurfaceBuffer, the GPU IR, …) so a cross-cutting change can't silently break the Smithay/wgpu path. Needs macOS + the nix dev shell (provides libxkbcommon).
	@[ "$$(uname)" = "Darwin" ] || { echo "mac-crates: macOS-only (dd-compositor links libxkbcommon + the Cocoa/Metal present path) — skipping on $$(uname). Maintainer: run via the mac bridge."; exit 0; }
	$(NIX_DEV) bash -euc '\
	  X="$$DD_LIBXKBCOMMON"; [ -n "$$X" ] || { echo "mac-crates: DD_LIBXKBCOMMON not exported by the dev shell — update nix/flake.nix"; exit 1; }; \
	  export RUSTFLAGS="-L native=$$X/lib $${RUSTFLAGS:-}" DYLD_LIBRARY_PATH="$$X/lib:$${DYLD_LIBRARY_PATH:-}"; \
	  cargo build -p dd-display -p dd-gpu-wgpu -p dd-compositor; \
	  cargo test  -p dd-compositor -p dd-gpu-wgpu'
perf: jit       ## same matrix + an oracle-vs-JIT slowdown table & summary (PERF_N=median runs; writes target/dd-tests/perf.{csv,json}); FILTER/ENGINE narrow
	PERF=1 cargo run -q -p dd-tests -- $(if $(ENGINE),-e $(ENGINE)) $(FILTER)
test-docker: jit ## end-to-end Docker-CLI scenarios against dd-daemon (run/logs/stop/kill/volumes/networks)
	bash dd-daemon/testdata/scenarios/docker.sh
test-docker-full: jit ## FULL Docker CLI/API compliance matrix (every command; maps each failure to a non-compliant verb)
	bash dd-daemon/testdata/scenarios/docker-full.sh
test-compose: jit ## end-to-end Docker Compose scenarios against dd-daemon (up/ps/logs/exec/down; skips if no compose)
	bash dd-daemon/testdata/scenarios/compose.sh
	bash dd-daemon/testdata/scenarios/compose-multinet.sh
test-docker-net: jit ## container-to-container networking (by-name DNS / by-IP / cross-network isolation)
	bash dd-daemon/testdata/scenarios/docker-net.sh
test-macos: jit ## macOS-container parity: same docker lifecycle on a Linux AND a native-macOS container
	bash dd-daemon/testdata/scenarios/macos-container.sh
test-realsw: jit ## run REAL pulled software (redis/python/postgres/nats) with deterministic workloads
	bash dd-daemon/testdata/scenarios/realsw.sh
test-smoke:     ## user-perspective: FRESH-PULL + run a real glibc distro on BOTH arches (the libc.so.6 guard; needs network, macOS)
	cargo build --release -p dd-cli -p dd-daemon
	bash dd-daemon/testdata/scenarios/smoke-realimage.sh
scenarios: jit  ## REAL software through dd-daemon (the SUT): popular images, both arches. CAT=databases TGT=arm to narrow
	cargo test -q -p dd-daemon --test scenarios -- --backend dd $(if $(CAT),-c $(CAT)) $(if $(TGT),-t $(TGT))
scenarios-real: ## same scenarios against the REAL docker oracle (mac Docker Desktop) — proves the tests are correct
	cargo test -q -p dd-daemon --test scenarios -- --backend real --long $(if $(CAT),-c $(CAT)) $(if $(TGT),-t $(TGT))
scenarios-long: jit ## full compatibility sweep against dd-daemon (pulls images, heavy workloads, both arches)
	cargo test -q -p dd-daemon --test scenarios -- --backend dd --long $(if $(CAT),-c $(CAT))
scenarios-count: ## list every scenario×target case + total (runs nothing) — proves the case count
	cargo test -q -p dd-daemon --test scenarios -- --count $(if $(CAT),-c $(CAT))
scenarios-clean: ## reap ONLY harness-spawned dd-daemons (by pidfile, mac-side) + remove scratch — keeps the host clean
	@for pf in target/dd-scen/*/daemon.pid; do [ -f "$$pf" ] || continue; pid=$$(cat "$$pf"); \
	  mac bash -lc "kill $$pid 2>/dev/null; true" </dev/null; echo "reaped $$pid"; done; \
	  rm -rf target/dd-scen/dd-* target/dd-scen/real-* target/dd-scen/*.sh 2>/dev/null; echo "scratch cleaned"
scenarios-prune: ## DISK reclaim: drop UNUSED images from the mac docker ORACLE (Docker Desktop). Removes anything no container uses; re-pulls on next run. Opt-in.
	@echo "oracle disk BEFORE:"; mac docker system df </dev/null 2>&1 | head -2; \
	  mac docker image prune -af </dev/null 2>&1 | tail -1; \
	  echo "oracle disk AFTER:"; mac docker system df </dev/null 2>&1 | head -2
coverage: jit  ## report unimplemented syscalls/opcodes (static switch-diff + dynamic corpus run); MODE=static|dynamic|all
	bash dd-tests/tools/coverage.sh $(or $(MODE),all)
bench: jit      ## TRUE DBT overhead: self-timed compute kernels (startup EXCLUDED) — native-arm64 vs dd-arm64/dd-x86/qemu-x86; BENCH_N=median (3), BENCH_K=alu,fp to narrow; writes target/dd-tests/bench.{csv,json}
	cargo run -q -p dd-tests --release --bin bench
# The decomposed C engine lives under dd-jit-darwin/src/runtime/{engine,translate,host,include,os,targets}
# (os/ covers both os/linux and os/darwin). Uses dd-jit-darwin/.clang-format.
RUNTIME_C = $(shell find dd-jit-darwin/src/runtime/engine dd-jit-darwin/src/runtime/translate dd-jit-darwin/src/runtime/host dd-jit-darwin/src/runtime/include dd-jit-darwin/src/runtime/os dd-jit-darwin/src/runtime/targets \( -name '*.c' -o -name '*.h' \))
fmt:            ## format the whole tree: clang-format the C engine + cargo fmt the Rust crates
	clang-format -i $(RUNTIME_C)
	cargo fmt --all
fmt-check:      ## CI: verify clang-format + cargo fmt are clean (no writes)
	clang-format --dry-run --Werror $(RUNTIME_C)
	cargo fmt --all -- --check
app:            ## build + assemble & ad-hoc-sign build/dd.app (the GTK GUI bundle; macOS)
	@chmod +x dd-gui/package/bundle.sh dd-gui/package/make-dmg.sh
	cargo clean -p dd-jit-darwin --release                 # FORCE a fresh C engine: build.rs's .c rerun-if-changed is unreliable under CI rust-cache, so a stale engine could otherwise ship (Rust/daemon fixes shipped while engine/C fixes silently didn't)
	cargo build --release -p dd-daemon -p dd-cli   # native toolchain: builds + allow-jit-signs the ddjit-* engines
	DD_VERSION=$(VERSION) $(NIX_DEV) dd-gui/package/bundle.sh $(VERSION)   # DD_VERSION -> baked into the dd-app binary
dmg: app        ## build dist/dd-<ver>-<arch>.dmg from the app bundle (macOS)
	$(NIX_DEV) dd-gui/package/make-dmg.sh $(VERSION)
install: app    ## copy the app to /Applications and run `dd install` (per-user, no root)
	rm -rf /Applications/dd.app && cp -R target/dd.app /Applications/
	cargo run -q -p dd-cli -- install
uninstall:      ## remove the daemon agent + docker context (keeps ~/.dd unless --purge)
	cargo run -q -p dd-cli -- uninstall
# --- macOS dev-container image (`ddcli mac`) -------------------------------------------------------
DDMAC_REPO ?= huttarichard/ddmac
DD_IMAGES  ?= $(HOME)/.dd/images
# docker pointed at the dd daemon socket (override if your socket lives elsewhere / use a context).
DD_DOCKER  ?= docker --host unix://$(HOME)/.dd/run/docker.sock
mac-image:      ## build the macOS dev-container images (base + dev) into $$DD_IMAGES (macOS + nix)
	DD_IMAGES=$(DD_IMAGES) bash dd-gui/mac/mac-image.sh base
	DD_IMAGES=$(DD_IMAGES) bash dd-gui/mac/mac-image.sh dev
	-cargo run -q -p dd-cli -- daemon restart   # re-discover the new images
mac-push: mac-image ## tag + push to $(DDMAC_REPO):{base,dev,latest}; needs DDMAC_TOKEN=<docker hub PAT>
	@test -n "$(DDMAC_TOKEN)" || { echo "set DDMAC_TOKEN=<docker hub PAT> (NEVER commit it); rotate after use"; exit 1; }
	@printf '%s' "$(DDMAC_TOKEN)" | $(DD_DOCKER) login -u huttarichard --password-stdin
	$(DD_DOCKER) tag ddmac-base $(DDMAC_REPO):base
	$(DD_DOCKER) tag ddmac-dev  $(DDMAC_REPO):dev
	$(DD_DOCKER) tag ddmac-dev  $(DDMAC_REPO):latest
	$(DD_DOCKER) push $(DDMAC_REPO):base
	$(DD_DOCKER) push $(DDMAC_REPO):dev
	$(DD_DOCKER) push $(DDMAC_REPO):latest
	@echo "pushed $(DDMAC_REPO):{base,dev,latest} — now: ddcli mac   (pulls $(DDMAC_REPO):latest)"

clean:
	cargo clean
