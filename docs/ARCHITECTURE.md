# hl architecture (target)

Composition-root design. **`hl` is the brain**; every other crate is a *provider* of
primitives, traits, or components. `hl` snaps them together like Lego and drives the runtime.

## Core idea

- **`hl` provides the *configuration*** — it defines the config model (which settings exist:
  workspace, vpn, cuda, …), holds their values, exposes on-change listeners / a clean API for
  user changes, provides the inner-layer communication, composes every provider, and **launches**
  (starts `hl-jit` and, per the resolved config, launches the terminal workspace).
- **The GUI turns that config into a real GUI** — it renders whatever config `hl` hands it, using
  generic components. It knows nothing about vpn/cuda/any feature.

So: `hl` = configuration + composition + launch. Everything else = reusable providers.
`hl` is platform-agnostic (macOS today; linux→linux later) — no host assumptions leak into it.

## Crates

| Crate | Role | Knows about |
|---|---|---|
| **`hl`** | entry point, composition root, config provider, launcher, packaging | everything (it composes) |
| **`hl-ws`** | workspace model + **traits** (`Launcher`, device/feature seams) — generic | nothing feature-specific (no vpn/cuda concrete types) |
| **`hl-ws-term`** | terminal primitive only (VT grid / input / CPU-render / pty) | just the terminal |
| **`hl-ws-gui`** | generic GUI **primitives**: reusable setting components (toggle/field/panel + a "define a settings section" API) | nothing — renders config into widgets |
| **feature providers** (vpn, cuda, …) | self-contained: config + trait impl (how it affects launch) + renderable via gui primitives | only their own domain |

## Rules

1. No cross-domain concrete type crosses a crate boundary — cross-domain concerns are **traits**.
2. A feature (vpn, cuda, …) is a plugin: it provides config, a launch-effect trait impl, and can be
   rendered by the gui primitives. `hl` decides which features exist and wires their listeners.
3. `hl-ws-gui` never references a feature by name; `hl` maps config → components.
4. `hl` owns platform specifics behind an abstraction so linux→linux support is additive, not a rewrite.
