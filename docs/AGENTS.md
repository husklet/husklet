# dd — build, run & contribute (the one doc every agent reads first)

If you are an AI agent or a new contributor, read this top to bottom before touching anything. This is
the single source of truth for what dd is, how to build it, how to run a container, and how to work
without breaking things. When in doubt, this file wins.

---

## 1. What dd is (the goal)

dd runs Linux containers natively on macOS, with no virtual machine. It is a dynamic binary translator
(DBT): it executes Linux `x86_64` and `aarch64` guest code on Apple Silicon by translating guest
instructions to ARM64 host code on the fly, and emulates the Linux syscall ABI on top of macOS. A
Docker-API daemon makes `docker run` / `docker build` / Compose work against it.

The mission: be a drop-in, fast replacement for Docker Desktop / a Linux VM on Mac.
- arm64 guests run at ~native speed (verbatim translation + hardware crypto).
- x86_64 guests are the differentiator: they run through our JIT, not a VM's emulation. Reaching parity
  with the arm path, via smart translation, is a first-class goal.

Success = "download it, `docker run` your image, it just works, and it's fast."

---

## 2. Architecture (one paragraph)

- `dd-jit/` — the engine (Rust shell + a large C core under `src/runtime/`). Translators in
  `runtime/translate/{x86_64,aarch64}/`; Linux syscall/container emulation in
  `runtime/os/linux/{syscall,container}/`; host/darwin glue in `runtime/os/darwin/`, `runtime/host/`.
  Builds two engines: `ddjit-linux_x86_64`, `ddjit-linux_aarch64`.
- `dd-daemon/` — the Docker Engine API server (Rust).
- `dd-cli/` — the `ddcli` binary: `run`, `mac`, `install`, `daemon`, `doctor`.
- `darwinjail` — a DYLD-interpose jail for the experimental macOS container (`ddcli mac`).
- `dd-tests/` — the correctness matrix (`make test`), scenarios, benches.

---

## 3. GOLDEN RULES — the three footguns that waste agent-hours

1. PIN `DDJIT_DIR` OR YOU WILL TEST THE STALE INSTALLED ENGINE. The engine resolves in this order
   (`dd-jit/src/lib.rs`): `$DDJIT_DIR` -> the path `build.rs` baked in -> `/Applications/dd.app/Contents/Resources`.
   Unset `DDJIT_DIR` can silently run the OLD installed engine and hide your change. After building:
   ```bash
   export DDJIT_DIR="$(ls -d <your-target-dir>/release/build/ddjit-*/out | tail -1)"
   ```

2. `cargo clean -p ddjit --release` BEFORE REBUILDING AFTER A C EDIT. `build.rs`'s `rerun-if-changed`
   does not track `#include`d `.c` files, so a plain rebuild can serve a cached engine without your change.

3. PARALLEL BUILDS MUST USE ISOLATED BUILD DIRS. Two builds sharing one `target/` poison each other's
   cache. Each concurrent worker: `--target-dir target-<yourname>` and pin `DDJIT_DIR` into that dir.

4. CODEGEN THAT BAKES A HOST POINTER MUST USE THE RECORDED EMITTERS. Any emitter that embeds a host
   address in emitted code (`e_movconst(.., (uint64_t)<host symbol>)`, `e_adrp_add` to an engine global)
   breaks the persistent translation cache: a restored arena lives at a different base in a different
   process, so an unrecorded raw bake becomes a garbage branch on a warm load (silent death, warm-only —
   the default matrix won't catch it; the pcachex group will). Use `emit_blockret`/`emit_ibtcptr`/
   `pc_record_*` (see `translate/*/pcache.c`) or add a recorded emitter. Review lint: grep new codegen
   for `e_movconst.*\(uint64_t\)` / `e_adrp_add.*g_` outside pcache.c.

> Maintainer/CI dev-env note (contributors can ignore this box): this repo may contain a local,
> gitignored `.dev/` with helper scripts (`.dev/build.sh`, `.dev/test.sh`) and a private playbook
> (`.dev/AGENTS.local.md`) that wrap the maintainer's specific build path. If `.dev/` exists, use those
> wrappers. They are not required to build or contribute; plain `make jit` on macOS is the supported path.

---

## COMPLETENESS — complete the surface, don't hot-patch (EVERY agent, including the manager)

Fixing one error by implementing one narrow case is NOT a solution. We do not ship half-working
subsystems. When a single failure triggers your task, your job is the whole surface behind it.

- Not acceptable: "cannot find /proc/sys/kernel/random/boot_id" -> add just `boot_id`.
  Required: implement the complete `/proc` + `/proc/sys` surface real software reads, correctly.
- Not acceptable: one unimplemented opcode aborts -> add just that opcode.
  Required: implement the whole opcode family/table (all forms, widths, prefixes, flags), byte-exact vs oracle.
- Not acceptable: one syscall returns ENOSYS -> stub just it.
  Required: cover the family (all related numbers + flag combinations) to real Linux semantics.

The rule: either (a) fully complete the subsystem this pass, or (b) enumerate the COMPLETE gap set, land
it in tracked structured batches, and drive it to zero. Never close a task as "done" while known gaps
remain. "Done" = the subsystem is complete and correct, not "the error went away." The deliverable is
implementation plus (if large) an executed completion plan, never a catalog of what is missing.

**Cross-platform is part of "complete."** Compliance and its tests must cover ALL THREE matrix engines:
`linux/x86_64`, `linux/aarch64`, and `darwin/aarch64` (the macOS container via darwinjail). A fix is not
done until it is correct — and tested — on every platform that can support the feature. Where the darwin
container uses a different mechanism than Linux (darwinjail + host FS instead of overlayfs, host
networking, etc.), implement the equivalent correct behavior there and add a darwin test. A per-platform
exclusion (e.g. an x86-only opcode) must be *justified in the test*, never silently assumed.

---

## 4. Build

On a real Mac (contributors / the community path). You are on the target platform; native `clang`
builds the engine. No bridges, no VM.
```bash
git clone <repo> && cd dd
make jit          # build both engines + the crates
make test         # correctness matrix
make install      # install the daemon + a `dd` docker context (no root)
```

Maintainer / CI dev env: builds through a local, gitignored `.dev/` helper (see the note in §3).
Contributors never need it. Nothing dev-env-specific (bridges, VMs, oracles) belongs in committed code
or docs; it lives under `.dev/` only.

Key make targets: `make jit` (build), `make test` (matrix; `FILTER=name ENGINE=x86_64` narrows),
`make bench` (self-timed DBT overhead), `make perf` (oracle-vs-JIT table), `make scenarios` (real images
through the daemon), `make coverage` (unimplemented syscalls/opcodes), `make app|dmg|install` (package).

---

## 5. Run a container

After `make install` (daemon + a `dd` docker context):
```bash
ddcli ubuntu                              # shell in a container, cwd mounted, host networking
ddcli run --platform linux/amd64 alpine sh  # force the x86_64 guest (via the JIT)
ddcli <image> [command…]                  # shorthand for `ddcli run`
ddcli mac                                 # experimental macOS container (darwin jail)
docker --context dd run redis             # the daemon speaks the Docker Engine API
ddcli doctor                              # diagnose socket / agent / context
```
Run a guest binary directly (no daemon; how benches/tests drive the engine): `ddjit::SpawnConfig::command`
(see `dd-tests/src/bin/bench.rs`). Guest binaries must live where the build host can see them (the shared
repo tree), not `/tmp`.

---

## 6. Validation — who runs what (speed matters)

Split the work so changes land fast:

- AGENTS run the MINIMUM: implement, then run only the targeted check for your change — your new
  differential test case, your one scenario, or the specific group (`make test FILTER=<yourcase>`). Do
  NOT run the full matrix; do not benchmark unrelated things. Leave changes UNCOMMITTED and report.
- THE MANAGER runs the full matrix (`make test`, both arches) in the BACKGROUND and merges. Agents are
  never blocked on it.

Any codegen/opcode/syscall change still needs a byte-exact differential vs the oracle (`qemu-x86_64` /
docker amd64) plus a PERMANENT test added under `dd-tests` so it can't regress. Perf changes report a
before/after from `make bench`/`make perf`. (Known worktree-only artifact: five `x86/{hello,glibc,
glibc-min,ctest,hx}` "fails" appear when a worktree lacks the prebuilt `poc/` fixtures — ignore them.)

---

## 7. How agents work here (workflow)

- One concern per agent, its own git worktree off `origin/main` (`.claude/worktrees/<branch>`), its own
  `--target-dir target-<name>`.
- Do the minimum to prove the change (§6). Leave changes UNCOMMITTED; the manager merges by explicit path
  (worktree bases often predate HEAD, so 3-way-merge shared files). No `git add -A`. No `Co-Authored-By`.
- Never edit canonical source directly; never touch `docs/TODO.md`.
- Clean up: reap every process/container/daemon you start, by explicit PID/ID. No `sudo`, no
  `pkill`/`killall` by name, no `rm -rf` on bare/variable paths. Don't touch other agents' worktrees/dirs.
- Everything gets a timeout. Obey COMPLETENESS above.

---

## 8. Manager playbook (the coordinator reads this)

The manager coordinates; it does not hand-implement except for trivial edits and merges.

- Delegate each gap to a minimal-scope agent in an isolated worktree + `--target-dir`. Give it enough
  context to build/run from day one (this doc + the specific gotchas). One concern per agent.
- Keep agents fast: they run only their targeted check. The manager runs the full matrix in the
  BACKGROUND (background command) and only blocks a MERGE on it, never an agent.
- Merge validated diffs by explicit path (3-way; bases predate HEAD). Batch several validated wins,
  rebuild the canonical engine ONCE per batch, run one background matrix, then commit + tag + push.
- Do not rebuild the canonical engine while a profiling agent pins it (`DDJIT_DIR` -> canonical out-dir);
  use isolated dirs for everything else so concurrent builds never poison each other.
- Enforce COMPLETENESS: reject scoped hot-patches; require whole-surface work or a tracked completion plan
  driven to zero.
- Prevent conflicts: agents editing the same file (e.g. `translate.c`, `completeness.rs`) must be merged
  one at a time with a 3-way merge; sequence them or resolve regions by hand.
- Keep canonical clean (only intended dirty files); watch for zombie/orphan engines and reap them.

---

## 9. Portability / community notes (don't bake our dev env into the product)

- OrbStack / Docker Desktop / the build bridge are the maintainer's dev convenience, NOT requirements. A
  contributor on a real Mac needs none of them. Never write code or docs that assume they exist at runtime.
- Outbound network from the dev host may be firewalled (VPN / per-app firewall) and can block the engine's
  egress. If a container's `apt`/`pip`/`npm` hangs on the dev machine, suspect the host firewall/VPN first;
  confirm, don't assume.
- User-facing behavior must match real Docker/Linux semantics; the oracle (`make scenarios-real`, `qemu`)
  is the arbiter of "correct".

---

Related: `README.md` (overview), `docs/TODO.md` (open work), `docs/coverage-gaps.md` (syscall/opcode gaps),
`docs/benchmarks*.csv` (perf).
