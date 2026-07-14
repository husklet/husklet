# Architecture & requirements (target)

Authoritative capture of the maintainer's direction (2026-07-13). The product brand is **husklet**;
the code prefix is **`hl`** everywhere.

## 1. Guiding principle

**`hl` is the brain — the composition root.** Every other crate is a *provider* of primitives, traits,
or components. `hl` snaps them together like Lego and drives the runtime. No cross-domain concrete type
crosses a crate boundary; cross-domain concerns are **traits**. Each crate keeps minimal dependencies.

**`hl` provides the *configuration*; the GUI turns that config into a real GUI.** `hl` declares what
settings exist, holds their values + on-change API, and maps them to engine primitives at launch. The GUI
renders whatever config `hl` gives it and never knows a feature by name.

## 2. Crates & dependency shape

`hl-ws` is the **minimal shared leaf** (std-only, zero `hl-*` deps). Edges point **up** to it.

```
                 hl-ws         std(+serde) only. The bare Workspace RUN primitive + shared traits
              ▲    ▲    ▲       (Launcher, terminal/pty interface, Arch, Mount).
              │    │    │
     hl-ws-term  hl-ws-gui   (each depends UP on hl-ws and implements its traits)
                       ▲
                       │
                      hl       composition root — depends on everything, composes + drives runtime
```

| Crate | Role | Knows about |
|---|---|---|
| **`hl`** | Entry point + composition root + config/settings owner + launcher + macOS packaging. Defines which settings exist; holds their values + on-change listeners; **extends** the bare Workspace with feature settings; **maps** each setting → its primitive at launch (vpn→hl-jit egress arg, cuda→hl-gpu, gui→compositor-socket primitive, docker_sock→mount); launches `hl-jit` + the terminal workspace. Owns `resolve_cli` + the platform seam. **Platform-agnostic** (macOS now, linux→linux later — no host assumptions leak in). | everything (it composes) |
| **`hl-ws`** | The bare-minimum **Workspace primitive** + the shared traits common to all `hl-ws-*`. std-only, zero hl-* deps. | nothing feature-specific |
| **`hl-ws-term`** | Terminal primitive only (VT grid / input / CPU-render / pty); implements hl-ws's terminal trait. Owns terminal settings (e.g. scrollback). | just the terminal |
| **`hl-ws-gui`** | Minimal generic GUI **primitives**: reusable setting components (toggle / field / panel + a "declare a settings section" API). Feature-shaped *render* components (e.g. a `VpnSettings` widget that renders inputs + reports edits back) are OK here — **rendering + input capture only, no launch/runtime logic.** | nothing — renders what hl gives it |
| **`hl-jit`** (engine) | Runs guests. Must expose **primitives** for features: vpn = an egress-socks **launch argument** (eventually a proper arg, not an env var); a device seam for GPU. | its own mechanism |
| **`hl-gpu`** | Host GPU IR + the CUDA device primitive (CudaDevice mechanism → Metal). | its own mechanism |

## 3. The Workspace primitive (hl-ws)

`hl-ws::Workspace` holds **only what is needed to run a workspace** — nothing more:
`name, image, arch, storage, shell, cpus, memory_mb, env, mounts`.

**Not in the model** (they are features / other packages' business): `docker_sock`, `gui`, `vpn`, `cuda`,
`scrollback`. `hl` owns a richer config = the bare Workspace **plus** feature settings.

## 4. Features (vpn, cuda, gui, docker_sock, …)

A feature is **not a crate** (no `hl-feature-*`). A feature is three things:
1. a **settings/config type** (data) — held by `hl` (e.g. `VpnConfig`, `CudaDevice` as plain data);
2. a **primitive** from the crate that owns the mechanism — `hl-jit` (vpn egress, gui compositor socket),
   `hl-gpu` (cuda);
3. **`hl` mapping** the setting → that primitive at launch.

The GUI (`hl-ws-gui`) provides a render component for the setting; `hl` decides the feature exists, holds
its value, listens for user changes, and passes it to the engine. `hl-ws` never sees a feature.

## 5. Platform seam (linux→linux, later Windows)

`hl` hides all host specifics behind a `Platform` abstraction (`state_root`, `app_bundle`, `open_app`,
`resolve_cli`, `install_service`, `prelaunch_env`, …): `MacPlatform` now, `LinuxPlatform` later — additive,
not a rewrite. The macOS GPU/present stack (`hl-display`, cfg-gated Metal/IOSurface/Cocoa/Mach) is the
model for how a platform-specific seam is isolated. Nothing macOS may leak into a host-neutral crate
(`hl-cli`/`hl`, `hl-daemon`, `hl-images`, `hl-jit`, `hl-gpu`, `hl-ws*`). Known leaks to fix: install/log/
service paths + `launchctl`/`open`/`xattr` in hl-cli; a hardcoded `/Users/x/.local/bin` in the gui.

## 6. Cross-cutting requirements

- **Rebrand:** `dd` → `hl` everywhere — `hl-*` packages, `hl_*` crates, `hl_`/`HL_` symbols + env vars,
  product brand `husklet`. (Done: packages, FFI + internal symbols. Pending: env `DD_*`→`HL_*`, bin/artifact
  names, state root `~/.dd`→fresh cutover, app/launchd/Docker/website, residue audit.)
- **No host-command shell-outs** for work Rust can do — implement natively (std::fs + a walk + `sha2`) or a
  real crate (`flate2` for gzip; the `tar` crate for archive extract, once vendorable). Legit external stay:
  git/gh, the docker CLI in daemon scenarios, the mac bridge, nix, engine/guest binaries, toolchain, launchctl.
- **Minimal env vars:** map every project env var to `HL_XXXX` **and reduce the count** — pass values
  directly (args/config) parent→child wherever possible; keep an env var only for a true cross-process
  contract or user override.
- **Drop darwin-GUEST support** (the darwinjail that runs macOS binaries as guests). Keep the macOS **host**.
- **Prefer passing values directly** over env/globals; keep crates dependency-lean.

## 7. Rules

1. No cross-domain concrete type crosses a crate boundary — use traits.
2. A feature = settings type + a primitive from the owning crate + hl mapping it. Not a crate.
3. `hl-ws-gui` never references a feature's *behavior*; it renders components. `hl` maps config → engine.
4. `hl-ws` stays minimal — only the run primitive + shared traits. Settings/config logic lives in `hl`.
5. `hl` owns platform specifics behind a seam so linux→linux is additive.
