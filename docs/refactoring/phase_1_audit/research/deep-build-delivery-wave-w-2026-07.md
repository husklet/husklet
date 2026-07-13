# Build, packaging and delivery audit — wave W (2026-07)

Documentation-only audit of the authoritative Make/workflow/package/install/build/cfg surfaces. No local
`AGENTS.md` or `.dev/AGENTS.local.md` exists in this worktree, so no additional repository-local policy
applied.

## Exact safe cuts and corrections

### Make target metadata

- `scenarios-prune` is a real target but is absent from `.PHONY`; add it. This is correctness metadata,
  not a new path.
- Remove `bench` from `.PHONY` and delete its recipe atomically with the already-approved benchmark island.
- `app` runs `chmod +x` on `bundle.sh` and `make-dmg.sh`, but both are tracked mode `100755`. The chmod is
  redundant in Git checkouts and can be removed without behavior or speed impact. Do not apply that logic
  to `mac-image.sh`/`mac-userland.sh`, which are tracked `100644` and intentionally invoked with `bash`.
- Target comments say `build/dd.app` and versioned `dist/dd-<ver>-<arch>.dmg`; scripts actually produce
  `target/dd.app` and `target/dist/dd.dmg`. Correct the comments rather than changing artifact paths relied
  on by release CI.

### Packaging script archaeology

`bundle.sh` and `make-dmg.sh` headers still instruct `tools/bundle.sh` / `tools/make-dmg.sh`, paths removed
when packaging moved in commit `5b1b68a7`; the active paths are `dd-gui/package/*.sh`. Update these four
examples/error messages. They are documentation-only dead paths.

`dd-gui/mac/README.md` has no exact repository link, but it is the human owner for `make mac-image` and
`make mac-push`; keep it. The signing README similarly has no runtime caller by design and documents the
live release secrets. Generated/vendored/human documentation must not be classified by executable refs.

## Build work that is produced then discarded

`bundle.sh` invokes `cargo build --release -p dd-gui`. Because `autobins=false` declares both `dd-app` and
`dd-term`, Cargo builds both. Packaging copies only `target/release/dd-app`; no package/install/CLI/GUI path
copies or launches `dd-term`. Its only documented consumer is a manual `cargo run --bin dd-term` command.

This creates an exact fork in product intent:

- if `dd-term` is a development-only binary, change packaging to `cargo build ... --bin dd-app`; this is a
  safe release-time reduction after confirming no build.rs side effect (there is none per-bin);
- if the terminal is a shipped product, copying/signing/launching it is missing packaging work and the
  build is not redundant.

Do not delete the binary based on packaging absence; the decision is migration-required. Current evidence
only proves its release build output is discarded.

## Compatibility versus truthful package completeness

The bundle requires all three JIT engines and correctly fails if one is absent. Preserve the forced
`cargo clean -p dd-jit-darwin --release`: its comment records a rust-cache/build.rs stale-C failure and the
cost occurs only during packaging, where freshness dominates speed.

By contrast, `darwinjail.dylib` and the Smithay compositor are optional in `bundle.sh`. Missing darwinjail
is logged and skipped although `ddcli mac` is advertised; compositor build failure is swallowed and the
legacy renderer is shipped. These are not dead branches. They are release-profile decisions that conflict
with a “full product/default compositor” goal. Once each feature is release-required, convert its skip path
to a hard failure and remove fallback prose. Until then, removing the branches would silently narrow the
package.

The loop copying either `dd` or `ddcli` is a rebrand compatibility seam. Cargo currently guarantees only
`ddcli`; keep the alias probe until rebrand selects canonical/compat names, then replace it with explicit
required artifacts so a missing CLI cannot yield a GUI-only bundle.

Optional icon/logo/starter-image copies are generated/asset seams. Each must be classified from product
manifest ownership, not Rust references. The app icon exists and is consumed; starter assets may be absent
in developer builds. Do not turn optional assets into deletions without bundle smoke evidence.

## CI and release

`pages.yml` is minimal and correctly publishes the checked-in static `website/` without rebuilding media;
this preserves deployment speed. Its dual `master, main` trigger is harmless compatibility unless branch
history proves `master` is permanently retired.

`smoke.yml` and `release.yml` both explicitly install floating stable Rust. `rust-toolchain.toml` also says
floating `stable`, so this is redundant rather than contradictory. Prefer one owner: remove workflow toolchain
installation only if GitHub actions reliably honors the file before cache key calculation, or pin a version
in the toolchain file and have CI consume it. Do not claim reproducibility while all three float.

Release instructions publish `dd install`/`dd app` although Cargo declares `ddcli`; this is retained rebrand
work, not a safe workflow cut. The release body also claims every release is signed/notarized while secrets
are explicitly optional and ad-hoc builds publish. Make the body conditional or prevent unsigned tag
publication; otherwise packaging state and user promise diverge.

The “Remove stale release assets” step is destructive but scoped to reruns of the same tag and ensures one
current asset. Keep it. Artifact upload plus GitHub Release upload are two intentional consumers (workflow
artifact and public release), not duplicate dead work.

## Feature/cfg findings

The audited crates define no Cargo feature table of their own in this partition; direct cfg branches are
primarily `test` and macOS/Linux platform gates. All observed non-test platform branches have consumers:
AppKit setup, macOS terminal spawn behavior, Linux capacity tests, and mac bridge provisioning. No cfg branch
is provably unreachable from supported targets.

`cfg!(target_os = "macos")` in cross-platform tests compiles both expression branches, unlike `#[cfg]`.
Do not delete apparently non-host code based on a Linux check. Conversely, `dd-daemon/src/test_support` is
crate-gated `#![cfg(test)]` and must not be treated as production package weight.

Dependency features (`dd-gpu` runtime, Tokio subsets, GTK version, VTE version) map to direct APIs or runtime
integration. No feature can be removed safely from textual evidence; validate with per-target builds before
narrowing.

## Consumer evidence and ordered actions

All primary delivery scripts are wired: Make calls both package scripts and mac-image; scenarios/tools call
mac-userland; shot.sh is the producer for GUI screenshot hooks; tools/dev.sh remains a developer entry point.
No build/install script is a proven whole-file deletion.

Strongest low-risk actions:

1. fix `.PHONY`, remove tracked-executable chmod, and correct stale artifact/script paths;
2. decide whether `dd-term` ships, then either package it or build only `dd-app`;
3. align canonical CLI/release instructions under rebrand;
4. make signing/notarization claims conditional and required-feature bundle failures truthful;
5. preserve cache freshness guards, static-site deployment, generated assets, and platform cfg paths.
