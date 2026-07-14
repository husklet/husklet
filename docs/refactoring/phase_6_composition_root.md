# Phase 6 — `hl` composition-root architecture

> Status: **design, not authorized to build.** Executes only after the phase-3 rebrand reaches a clean
> green all-`hl` (husklet) baseline. North star: [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) +
> memory `hl-crate-architecture`. Same non-negotiable rules as the rest of `docs/refactoring/`.

This document turns the target in `docs/ARCHITECTURE.md` into a concrete crate DAG, trait signatures, a
type-relocation table, the VPN/CUDA plugin shape, the platform seam, and an ordered migration plan whose
every step keeps `cargo build` + the full matrix green.

---

## 0. What exists today (baseline being restructured)

| Crate | Contains (relevant) |
|---|---|
| `hl-term` (lib `hl_term`; libc+bitflags) | terminal primitive: `vt grid input layout render font png session config pty` **+** `workspace.rs` (`Arch`, `Mount`, `VpnKind`, `VpnConfig`, `CudaDevice`, `Workspace`, `WorkspaceStore`, `Launcher`, `LocalShellLauncher`) |
| `hl-cli` (bin `ddcli`) | `main cli` (clap) · `workspace.rs` (`build_workspace`, CRUD, `run_inline` PTY passthrough, `RawMode`) · `hl_launcher.rs` (the real dd-jit `Launcher` + `DdJitPty`, inline gui/cuda resolution) · `paths.rs` (mac paths) · `app.rs` (`open` bundle) · `daemon install context doctor agent report run wsdaemon` |
| `hl-gui` (bins `dd-app`, `dd-term`) | `main.rs` (`AppModel`, container manager) · `bin/term.rs` (3.9k-line workspace terminal window) · `ui/views/{workspaces,settings,…}` · `ui/components/widgets/*` · `mac.rs` (AppKit titlebar) · `install.rs::resolve_cli` (dup in `views/workspaces.rs::ddcli_bin`) |
| `hl-gpu` (lib `hl_gpu`; pure std, `runtime`→hl-jit) | GPU IR/wire/backend · `cuda.rs` (`CudaDeviceDesc`, `CudaContext`, `PtxModule`) · `integration.rs` (`GpuIntegration`/`DisplayIntegration`/`CudaIntegration` implementing `hl_jit::DeviceProvider`) |
| `hl-jit` | `runtime/device.rs`: `DeviceProvider` / `DeviceRequest` / `DeviceMount` runtime-neutral seam; `ContainerBuilder::{bind,guest_env,egress_socks,cpus,memory_mb,apply_device}` |

Key smells this phase fixes:
- `hl-term` is two things at once (terminal primitive **and** workspace/feature model).
- `VpnConfig`/`CudaDevice` are **concrete** cross-domain types living in the terminal crate.
- The composition root is **split** across `hl-cli` (main/launcher/paths) and `hl-gui` (main/mac/term).
- GUI settings views hard-code feature knowledge (`ARCH_TOKENS`, vpn/cuda strings).
- macOS specifics (`/Applications/dd.app`, `~/.dd`, `open`, AppKit, objc fork-safety, `DD_GPU_POOL`) are
  scattered, not behind a seam.

---

## 1. Target crate DAG (no cycles; primitives feature-agnostic)

```
        libc, bitflags
              │
      ┌───────▼────────┐
      │  hl-ws-term    │   terminal primitive: VT/grid/input/render/pty  (was hl-term − workspace)
      └───────┬────────┘
              │ (PtyBackend)
      ┌───────▼────────┐
      │     hl-ws      │   Workspace model + persistence + TRAITS + settings SCHEMA (pure data)
      └──┬─────────┬───┘   deps: hl-ws-term. NO gtk/gpu/cuda/vpn/hl-jit.
         │         │
   ┌─────▼───┐  ┌──▼───────────┐        ┌───────────────┐
   │hl-ws-gui│  │ feature crates│        │    hl-jit     │  runtime; DeviceProvider/DeviceRequest seam
   │ (gtk)   │  │  hl-feat-vpn ─┤        └───────┬───────┘  (unchanged, still neutral)
   └─────┬───┘  │  hl-gpu ──────┤  (hl-gpu also →hl-jit runtime feature)
         │      └──────┬────────┘                │
         │             │                         │
         └──────────┬──┴───────────┬─────────────┘
                    ▼              ▼
              ┌───────────────────────────────┐
              │              hl               │  entry · config provider · launcher · packaging
              │  (was hl-cli, expanded; hosts │  deps: ALL of the above + hl-images + hl-client
              │   both GUI binaries)          │
              └───────────────────────────────┘
```

Edges (acyclic, verified against manifests):

- `hl-ws-term` → {libc, bitflags} only. Leaf.
- `hl-ws` → `hl-ws-term` (for `PtyBackend`, the `Launcher` return type). Pure otherwise.
- `hl-ws-gui` → `hl-ws` (schema types) + gtk4/relm4/vte4. **No** feature-crate edge.
- `hl-feature-vpn` → `hl-ws`. Pure.
- `hl-gpu` → `hl-ws` (implements `Feature`) + `hl-jit` (under `runtime`, for the `DeviceProvider` bridge).
- `hl` → hl-ws, hl-ws-term, hl-ws-gui, hl-feature-vpn, hl-gpu, hl-jit, hl-images, hl-client.
- Nothing depends on `hl`. `hl-ws*` never depend on any feature crate or hl-jit.

**Feature-agnostic proof.** `grep -R "vpn\|cuda\|nvml\|wayland\|iosurface" hl-ws hl-ws-term hl-ws-gui/src`
must return **zero** domain hits after the migration. Those crates depend on no feature crate, so they
cannot name a feature; the compiler enforces it. Only `hl` links the feature crates.

---

## 2. Trait interfaces to define (Rust signatures)

All of these are **pure data / pure traits** and live in `hl-ws` unless noted, so features and the GUI can
speak them without pulling gtk or hl-jit.

### 2.1 `Launcher` (moves from `hl-term::workspace`, unchanged)

```rust
// hl-ws/src/launch.rs
use hl_ws_term::PtyBackend;
use std::io;

/// Turn a configured Workspace into a live terminal.
pub trait Launcher {
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> io::Result<Box<dyn PtyBackend>>;
}

/// Host-shell fallback (tests + non-engine hosts). Real impl (`DdJitLauncher`) lives in `hl`.
pub struct LocalShellLauncher { pub shell: Vec<String> }
impl Launcher for LocalShellLauncher { /* … as today … */ }
```

### 2.2 Engine-neutral launch effect (new, in `hl-ws`)

`hl-ws` may know the **engine's launch primitives** (mount / env / egress / render-node / caps) — that is
in-domain for a workspace-launch layer — but it must not know any *feature*. This is the vocabulary a
feature speaks; it deliberately mirrors `hl_jit::DeviceRequest` **without depending on hl-jit**.

```rust
// hl-ws/src/effect.rs
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindMount { pub host: String, pub container: String, pub ro: bool }

/// Coarse engine capabilities a feature can request. Enumerated (not open) because the engine supports a
/// fixed set; adding one is an engine change, not a feature change. None of these names a feature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineCaps {
    /// Ask the engine to synthesize a host-backed render node (the accelerated GPU rung).
    pub render_node: bool,
    /// Redirect genuine external TCP through this SOCKS5 `host:port`.
    pub egress_socks: Option<String>,
}

/// Everything one feature contributes to a launch, in engine-neutral terms. `hl` folds every feature's
/// effect into one and applies it to the hl-jit builder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchEffect {
    pub mounts: Vec<BindMount>,
    pub env: Vec<(String, String)>,
    pub caps: EngineCaps,
}

impl LaunchEffect {
    pub fn merge(&mut self, other: LaunchEffect) { /* extend mounts+env, OR caps */ }
}
```

### 2.3 Settings schema (new, in `hl-ws`; pure data, NO gtk)

The feature-agnostic description the GUI renders. Lives in `hl-ws` so a feature can emit a schema without a
gtk dependency; `hl-ws-gui` depends on `hl-ws` to render it.

```rust
// hl-ws/src/settings.rs
pub enum SettingKind {
    Toggle,
    Text   { placeholder: String },
    Choice { options: Vec<String> },
    Int    { min: i64, max: i64 },
}
pub enum SettingValue { Bool(bool), Text(String), Choice(usize), Int(i64) }

pub struct Setting {
    pub key:   String,          // opaque, feature-scoped, e.g. "vpn.endpoint" — GUI never interprets it
    pub label: String,
    pub help:  Option<String>,
    pub kind:  SettingKind,
    pub value: SettingValue,    // current value
}
pub struct SettingsSection { pub title: String, pub settings: Vec<Setting> }
```

### 2.4 `Feature` — the plugin trait (new, in `hl-ws`)

This is the generalization of the memo's "device/GPU-presentation trait": one object owns a feature's
config parsing, its settings UI schema, and its launch effect. It is pure (no gtk, no hl-jit).

```rust
// hl-ws/src/feature.rs
/// Resolved host paths a feature needs to build its launch effect. `hl` (the composition root) owns
/// "where host files live" and fills this; the feature owns "how it maps into a guest". Mirrors today's
/// split (hl-cli resolves ~/.dd/… paths, hl-gpu composes the DeviceRequest).
pub struct HostContext<'a> {
    pub state_root: &'a std::path::Path,   // ~/.dd (mac) / … (linux)
    pub images_dir: &'a std::path::Path,
    pub guest_libdir: &'a str,             // /usr/lib/<arch>-linux-gnu
    pub overlay_libdir: &'a str,           // workspace upper's multiarch dir (for self-heal), or ""
    pub display_sock: &'a str,             // resolved compositor socket, or ""
    pub gpu_exec_sock: &'a str,            // resolved IR-executor socket, or ""
}

pub trait Feature {
    /// Stable id == persistence key == settings-key prefix, e.g. "vpn", "cuda", "display".
    fn id(&self) -> &'static str;

    /// The settings section to render for this workspace, or None if the feature offers no UI here.
    fn settings_section(&self, ws: &Workspace) -> Option<SettingsSection>;

    /// Apply one GUI edit (a key this feature owns) back onto the workspace's opaque feature config.
    /// Returns whether anything changed. `hl` persists + fires listeners after a `true`.
    fn apply_edit(&self, ws: &mut Workspace, key: &str, value: &SettingValue) -> bool;

    /// This feature's effect on a launch, or None if inert for this workspace. Pure w.r.t. the runtime.
    fn launch_effect(&self, ws: &Workspace, ctx: &HostContext) -> Option<LaunchEffect>;
}
```

### 2.5 Generic Workspace (feature config becomes an opaque bag)

`Workspace` loses its `vpn: Option<VpnConfig>` / `cuda: Option<CudaDevice>` fields. Feature config becomes
an opaque, feature-owned string keyed by feature id — so `hl-ws` never names a feature and the store never
parses one.

```rust
// hl-ws/src/model.rs  (was hl-term/src/workspace.rs)
pub struct Workspace {
    pub name: String, pub image: String, pub arch: Arch,
    pub storage: Option<PathBuf>, pub shell: Option<String>,
    pub cpus: Option<u32>, pub memory_mb: Option<u32>,
    pub env: Vec<(String, String)>, pub mounts: Vec<Mount>,
    pub docker_sock: bool, pub scrollback: Option<u64>,
    /// Per-feature persisted config, keyed by Feature::id(). The value is the feature's own spec string
    /// (round-trips through the feature's parser). hl-ws stores/loads it verbatim, never interpreting it.
    pub features: std::collections::BTreeMap<String, String>,
}
impl Workspace {
    pub fn feature(&self, id: &str) -> Option<&str> { self.features.get(id).map(String::as_str) }
    pub fn set_feature(&mut self, id: &str, spec: Option<String>) {
        match spec { Some(s) => { self.features.insert(id.into(), s); } None => { self.features.remove(id); } }
    }
}
```

`WorkspaceStore` serializes each `features[id]` as a `feature.<id> = <spec>` line, **and on load maps the
legacy `vpn = …` / `cuda = …` / `gui = true` keys into `features["vpn"|"cuda"|"display"]`** so existing
`~/.dd/workspaces.conf` files keep working (persisted-data compat rule).

### 2.6 Config provider + on-change listeners (new, in `hl`)

`hl` is the configuration authority: it holds the live `Workspace`, exposes a clean mutation API, and fans
edits out to listeners (persist, re-render, re-arm the launch).

```rust
// hl/src/config.rs
pub enum ConfigChange { Field(&'static str), Feature(String) }

pub struct Config {
    ws: Workspace,
    features: Vec<Box<dyn Feature>>,           // the registry `hl` composed
    listeners: Vec<Box<dyn Fn(&Config, &ConfigChange)>>,
}
impl Config {
    pub fn workspace(&self) -> &Workspace { &self.ws }
    pub fn on_change(&mut self, f: impl Fn(&Config, &ConfigChange) + 'static) { self.listeners.push(Box::new(f)); }

    /// Route a GUI edit to the owning feature (by key prefix), then notify.
    pub fn apply_edit(&mut self, key: &str, value: SettingValue) {
        if let Some(feat) = self.features.iter().find(|f| key.starts_with(f.id())) {
            if feat.apply_edit(&mut self.ws, key, &value) { self.notify(&ConfigChange::Feature(feat.id().into())); }
        }
    }
    /// Build the full settings UI schema from every registered feature (feature-agnostic output).
    pub fn settings_sections(&self) -> Vec<SettingsSection> {
        self.features.iter().filter_map(|f| f.settings_section(&self.ws)).collect()
    }
    fn notify(&self, c: &ConfigChange) { for l in &self.listeners { l(self, c); } }
}
```

### 2.7 Config → widgets rendering API (new, in `hl-ws-gui`)

`hl-ws-gui` turns a `SettingsSection` into real widgets and reports edits — knowing nothing about which
feature produced them.

```rust
// hl-ws-gui/src/settings.rs
use hl_ws::settings::{SettingsSection, SettingValue};

/// Receives edits from rendered widgets. `hl` implements this to forward into `Config::apply_edit`.
pub trait SettingsSink { fn on_edit(&self, key: &str, value: SettingValue); }

/// Render one section into `container` using the generic primitives; wire each widget's change signal to
/// `sink.on_edit(setting.key, …)`. Never inspects `key`.
pub fn render_settings_section(container: &gtk::Box, section: &SettingsSection, sink: std::rc::Rc<dyn SettingsSink>);

// Reusable primitives (extracted from today's hl-gui widgets) the renderer is built from:
pub fn toggle_row(label: &str, help: Option<&str>, on: bool) -> (gtk::Box, gtk::Switch);
pub fn text_row(label: &str, placeholder: &str, value: &str) -> (gtk::Box, gtk::Entry);
pub fn choice_row(label: &str, options: &[String], selected: usize) -> (gtk::Box, gtk::DropDown);
pub fn setting_panel(title: &str) -> gtk::Box;  // the "define a settings section" container
```

Flow: `hl` calls `config.settings_sections()` → hands each to
`hl_ws_gui::render_settings_section(box, section, sink)` → widget edit → `sink.on_edit` →
`config.apply_edit` → owning feature mutates the workspace → listener persists + re-arms. No feature name
crosses `hl-ws-gui`.

### 2.8 hl-jit bridge (retained; fed by `hl`)

`hl-jit`'s `DeviceProvider`/`DeviceRequest` stays exactly as-is (the runtime-neutral seam). `hl` aggregates
all `LaunchEffect`s and applies them:

```rust
// hl/src/launcher/apply.rs
fn apply(builder: ContainerBuilder, eff: &LaunchEffect) -> ContainerBuilder {
    let mut b = builder;
    for m in &eff.mounts { b = b.bind(m.host.clone(), m.container.clone(), m.ro); }
    if let Some(s) = &eff.caps.egress_socks { b = b.egress_socks(s.clone()); }
    if eff.caps.render_node || !eff.env.is_empty() || !eff.mounts.is_empty() {
        // Feed the runtime seam a plain request (render_node + env); mounts already bound above, or
        // route them through DeviceRequest — either is byte-equivalent to today's apply_device.
        b = b.apply_device(&DeviceRequest { render_node: eff.caps.render_node, env: env_lines(eff), mounts: vec![] });
    }
    b
}
```

---

## 3. Where every current type/function goes

| Current location | Symbol | Target |
|---|---|---|
| `hl-term/src/{vt,grid,input,layout,render,font,png,session,config,pty}.rs` | terminal primitive | **`hl-ws-term`** (rename of `hl-term`, lib `hl_ws_term`) |
| `hl-term/src/workspace.rs` | `Arch`, `Mount` | **`hl-ws`** `model.rs` |
| ″ | `Workspace` (minus `vpn`/`cuda` fields, plus `features` bag) | **`hl-ws`** `model.rs` |
| ″ | `WorkspaceStore`, `WsBuilder`, `kv/clean/sanitize` | **`hl-ws`** `store.rs` |
| ″ | `Launcher`, `LocalShellLauncher` | **`hl-ws`** `launch.rs` |
| ″ | `VpnKind`, `VpnConfig` | **`hl-feature-vpn`** `lib.rs` |
| ″ | `CudaDevice` | **`hl-gpu`** `cuda.rs` (fold into `CudaDeviceDesc` presentation) |
| `Workspace.vpn` / `Workspace.cuda` / `Workspace.gui` | fields | **`hl-ws`** `Workspace.features["vpn"|"cuda"|"display"]` |
| `hl-cli/src/main.rs`, `cli.rs` | entry + clap | **`hl`** `main.rs`, `cli.rs` |
| `hl-cli/src/workspace.rs` | `build_workspace`, `create/list/rm/launch/checkpoint` | **`hl`** `workspace_cmd.rs` (uses `Config` + feature registry for vpn/cuda) |
| ″ | `run_inline`, `RawMode`, `term_size` | **`hl`** `workspace_cmd.rs` (terminal passthrough; stays composition-root) |
| `hl-cli/src/hl_launcher.rs` | `launch_ex`, `DdJitPty`, `guest_of`, `want_arch`, `split_ref` | **`hl`** `launcher/ddjit.rs` — becomes the real `Launcher` impl |
| ″ | inline `gui`/`cuda`/`DD_GPU_POOL`/socket-path resolution | **removed** → moves into `hl-gpu`'s `Feature` impls + `HostContext` |
| ″ | `guest_is_musl`, `select_gui_lib_dir` | **`hl-gpu`** display feature (it owns shim ABI matching) |
| `hl-cli/src/paths.rs` | all path fns + `AGENT_LABEL`, `APP_BUNDLE` | **`hl`** `platform/mac.rs` (`MacPlatform` impl) |
| `hl-cli/src/app.rs` | `cmd_app` (`open`) | **`hl`** via `Platform::open_app` |
| `hl-cli/src/{daemon,install,context,doctor,agent,report,run,wsdaemon}.rs` | daemon/install/docker glue | **`hl`** (composition-root-owned; unchanged) |
| `hl-gpu/src/integration.rs` | `CudaIntegration` (+`GpuIntegration` cuda arm) | **`hl-gpu`** `CudaFeature: Feature` → `LaunchEffect` |
| ″ | `DisplayIntegration`, `shim_owns_lib`, `prune_shadowing_stubs` | **`hl-gpu`** `DisplayFeature: Feature` → `LaunchEffect` (`render_node` cap) |
| ″ | `GpuIntegration`/`DeviceProvider` impl | **retire** in favor of `hl` aggregating `LaunchEffect`→`DeviceRequest` (bridge in `hl`) |
| `hl-gpu/src/cuda.rs`, `ptx.rs`, IR/wire/backend | executor core | **stays `hl-gpu`** |
| `hl-jit/src/runtime/device.rs` | `DeviceProvider/DeviceRequest/DeviceMount` | **stays `hl-jit`** (fed by `hl`) |
| `hl-gui/src/ui/components/widgets/*`, `setting_card/action_row` | reusable widgets | **`hl-ws-gui`** primitives |
| `hl-gui/src/ui/views/settings.rs`, `workspaces.rs` | settings/workspace views | **`hl`** builds `SettingsSection`s → **`hl-ws-gui`** renders |
| `hl-gui/src/main.rs` (`AppModel`, container manager) | the `dd-app` shell | **`hl`** (composition root hosts the GUI binary; views via `hl-ws-gui`) |
| `hl-gui/src/bin/term.rs` (`dd-term` workspace window) | workspace terminal app | **`hl`** (uses `hl-ws` model + `hl-ws-gui` primitives + `hl-ws-term`) |
| `hl-gui/src/mac.rs` (AppKit titlebar) | native titlebar | **`hl`** `platform/mac.rs` |
| `hl-gui/src/install.rs::resolve_cli` + `views/workspaces.rs::ddcli_bin` (dup) | CLI-binary resolution | **`hl`** `platform` (single copy) |

> Note on "resolve_cli + mac-target launch logic currently in hl-term": in the live tree `resolve_cli`
> is in `hl-gui` (duplicated), and the "mac target" is `Arch::DarwinArm64` + `guest_of` in
> `hl-term::workspace`/`hl-cli::hl_launcher`. `Arch` (incl. `DarwinArm64`) stays in `hl-ws`; the darwin
> **launch** path lands in `hl`'s `Launcher` impl; `resolve_cli` lands in `hl::platform`.

---

## 4. VPN and CUDA as feature plugins (concretely)

### 4.1 `hl-feature-vpn` (pure, deps: `hl-ws`)

```rust
pub struct VpnKind…; pub struct VpnConfig { kind, endpoint }   // moved verbatim from hl-term

pub struct VpnFeature;
impl Feature for VpnFeature {
    fn id(&self) -> &'static str { "vpn" }

    fn settings_section(&self, ws: &Workspace) -> Option<SettingsSection> {
        let cfg = ws.feature("vpn").and_then(VpnConfig::parse);
        Some(SettingsSection { title: "Network (VPN egress)".into(), settings: vec![
            Setting { key: "vpn.kind".into(), label: "Proxy kind".into(), kind: SettingKind::Choice {
                        options: vec!["off".into(),"socks5".into(),"http".into(),"wireguard".into(),"openvpn".into()] },
                      value: SettingValue::Choice(kind_index(&cfg)), help: None },
            Setting { key: "vpn.endpoint".into(), label: "Endpoint".into(),
                      kind: SettingKind::Text { placeholder: "host:port or /path/wg.conf".into() },
                      value: SettingValue::Text(cfg.map(|c| c.endpoint).unwrap_or_default()), help: None },
        ] })
    }

    fn apply_edit(&self, ws: &mut Workspace, key: &str, v: &SettingValue) -> bool {
        // merge the edited field into the current VpnConfig, re-serialize to ws.set_feature("vpn", …)
    }

    fn launch_effect(&self, ws: &Workspace, _ctx: &HostContext) -> Option<LaunchEffect> {
        let cfg = VpnConfig::parse(ws.feature("vpn")?)?;
        let socks = cfg.socks_endpoint()?;   // only socks5 resolves directly today
        Some(LaunchEffect { caps: EngineCaps { egress_socks: Some(socks.into()), ..Default::default() }, ..Default::default() })
    }
}
```

Replaces today's inline `if let Some(vpn) = &ws.vpn { builder.egress_socks(...) }` in `hl_launcher.rs`.

### 4.2 CUDA feature (in `hl-gpu`, deps: `hl-ws` + `hl-jit`)

`CudaDevice` (presentation: name/cc/vram) folds into `hl-gpu`. `CudaIntegration`'s NVML/nvidia-smi injection
becomes the feature's `launch_effect`, resolving drop-ins via `HostContext.state_root` (the path resolution
that lived in `hl-cli`):

```rust
pub struct CudaFeature;
impl Feature for CudaFeature {
    fn id(&self) -> &'static str { "cuda" }
    fn settings_section(&self, ws) -> …   // name(Text), compute_capability(Text), vram_mb(Int), enable(Toggle)
    fn apply_edit(&self, ws, key, v) -> … // → ws.set_feature("cuda", CudaDevice{…}.to_spec())
    fn launch_effect(&self, ws, ctx) -> Option<LaunchEffect> {
        let d = CudaDevice::parse(ws.feature("cuda")?)?;
        // resolve nvml_so/nvidia_smi under ctx.state_root (was hl-cli); build mounts+env exactly as
        // CudaIntegration does today. No render_node (matches `cuda_only_…` test).
    }
}
```

### 4.3 Display/GUI feature (in `hl-gpu`)

`gui: bool` becomes `features["display"]` presence. `DisplayFeature::launch_effect` reproduces
`DisplayIntegration::device_request` byte-for-byte (sockets, shim mounts filtered by `shim_owns_lib`,
overlay stub pruning, `LD_LIBRARY_PATH`) and sets `caps.render_node = true`. `guest_is_musl`/
`select_gui_lib_dir` move here (shim ABI matching is display-domain). The `DD_GPU_POOL` fork-safety seeding
is **host**-domain → `Platform::prelaunch_env` (§5), not the feature.

The existing `hl-gpu/src/integration.rs` **tests** (`display_only_…`, `cuda_only_…`, `prune_*`,
`shim_owns_*`) are the regression net: re-point them at the `Feature`/`LaunchEffect` output; they must stay
green byte-for-byte.

---

## 5. Platform-abstraction seam (linux is additive)

New `hl/src/platform/` with a `Platform` trait that hides every host assumption; `MacPlatform` today,
`LinuxPlatform` a stub whose later completion is purely additive.

```rust
// hl/src/platform/mod.rs
pub trait Platform {
    fn state_root(&self) -> PathBuf;              // ~/.dd (mac) | ~/.local/share/husklet (linux)
    fn images_dir(&self) -> PathBuf;
    fn logs_dir(&self) -> PathBuf;
    fn app_bundle(&self) -> Option<PathBuf>;      // Some(/Applications/husklet.app) | None
    fn open_app(&self, p: &Path) -> io::Result<()>;          // `open` | xdg-open / exec
    fn resolve_cli(&self) -> Option<PathBuf>;               // bundle Resources | sibling | PATH
    fn install_service(&self) -> io::Result<()>;            // launchd plist | systemd unit
    fn uninstall_service(&self) -> io::Result<()>;
    /// Host/engine knobs applied to a launch's process env before spawning the engine — the mac-only
    /// objc fork-safety + IOSurface pool seeding live here (no-op on linux).
    fn prelaunch_env(&self, ws: &Workspace, env: &mut Vec<(String, String)>);
    /// Native window chrome hook (AppKit titlebar on mac; no-op elsewhere).
    fn decorate_window(&self, win: &gtk::Window);
}
```

What it hides (currently hard-coded): `paths.rs` constants → `MacPlatform`; `app.rs`'s `open` →
`open_app`; `hl-gui/mac.rs` AppKit → `decorate_window`; `resolve_cli`/`ddcli_bin` dup → `resolve_cli`;
`OBJC_DISABLE_INITIALIZE_FORK_SAFETY` + `DD_GPU_POOL` derivation in `hl_launcher.rs` → `prelaunch_env`;
`AGENT_LABEL`/launchd → `install_service`. `Arch::DarwinArm64` stays a workspace concept in `hl-ws`; the
engine's guest mapping (`guest_of`) stays in `hl`'s launcher.

Guardrail: `hl` code outside `platform/` may not reference `/Applications`, `~/Library`, `open`, `objc`,
or `launchd`. A CI grep enforces it, so linux support is adding a `LinuxPlatform`, not editing the core.

---

## 6. Ordered migration plan (post-rebrand; each step = one commit + gate)

Precondition **P0**: rebrand to husklet complete, all crates `hl-*`, `cargo build` (default-members) +
`.dev/test.sh` full matrix green. Capture that as the baseline to diff against (memory
`full-harness-after-merges`). Every step below coordinates crate renames with codex on the shared tree
(memory `shared-main-tree-codex`) — do renames when the tree is otherwise quiet.

**Gate (all steps):** `cargo build` (default-members, Linux) + `cargo build -p hl` / `-p hl-ws-gui` on mac
+ `cargo test -p <touched>` + the relevant `.dev/test.sh FILTER=…` case; a **full** `.dev/test.sh` on the
structural steps (1, 6, 9). Validate GUI-visible steps with `DD_SHOT`.

1. **Split the terminal from the workspace.** Create `hl-ws`; move `workspace.rs` into it (vpn/cuda still
   concrete for now — pure move). `hl-ws` deps `hl-term`. Update importers (`hl-cli`, `hl-gui` × 5 files)
   `hl_term::workspace` → `hl_ws::workspace`. *Gate:* `hl-ws`/`hl-ws-term` unit tests + full matrix.
2. **Rename `hl-term` → `hl-ws-term`** (lib `hl_ws_term`). Cosmetic; update the two dependents. Separate
   commit so a test regression is attributable. *Gate:* build + matrix.
3. **Introduce the neutral model** in `hl-ws`: `LaunchEffect`/`EngineCaps`/`BindMount`, settings schema,
   `Feature` trait, `HostContext`. Add `Workspace.features` bag **alongside** existing `vpn`/`cuda`/`gui`
   fields (additive, both persisted). *Gate:* store round-trip test incl. legacy-key read.
4. **VPN → plugin.** Create `hl-feature-vpn`; move `VpnKind`/`VpnConfig`; impl `Feature`. Store loads
   legacy `vpn=` into `features["vpn"]`; drop `Workspace.vpn`. `hl` sources vpn effect from the feature
   (replaces the inline `egress_socks` block). *Gate:* vpn matrix case (socks egress) + three-way-semantics
   tests re-homed.
5. **CUDA → plugin.** Move `CudaDevice` into `hl-gpu`; add `CudaFeature`; port `CudaIntegration` path
   resolution from `hl-cli` into the feature via `HostContext`. Drop `Workspace.cuda`. *Gate:* the
   `cuda_only_*` / missing-shim tests re-pointed at `LaunchEffect`; cuda matrix case.
6. **Display → plugin + launcher aggregation.** Add `DisplayFeature` (port `DisplayIntegration`,
   `shim_owns_lib`, `prune_shadowing_stubs`, `select_gui_lib_dir`, `guest_is_musl`). `gui` → 
   `features["display"]`. `hl` composes all `Feature::launch_effect`s → one `LaunchEffect` → hl-jit builder;
   delete the inline gui/cuda blocks in the launcher. *Gate:* **full** render+cuda+vpn matrix, byte-diff the
   composed `DeviceRequest` against baseline (the integration tests are the oracle). Retire
   `GpuIntegration`'s `DeviceProvider` impl.
7. **Composition root.** Rename `hl-cli` → `hl` (bin stays the CLI). Introduce `Platform` +
   `MacPlatform` (fold `paths.rs`, `app.rs`, `hl-gui/mac.rs`, `resolve_cli`). *Gate:* `doctor`/`install`
   smoke + build.
8. **Config provider.** Add `hl::Config` (holds `Workspace` + feature registry + `on_change`); route
   `build_workspace` (CLI create) and future GUI edits through it. *Gate:* `Config` unit tests; CLI
   create/list/rm behavior unchanged.
9. **GUI primitives.** Rename `hl-gui` → `hl-ws-gui`; extract generic widgets + `render_settings_section`
   + `SettingsSink`. Move the app shells (`AppModel`, `bin/term.rs`) into `hl`. Rewrite the settings/
   workspaces views as `Config::settings_sections()` → `hl_ws_gui::render_settings_section`. *Gate:*
   `DD_SHOT` screenshots of the settings + workspaces panels match; `dd-term` launches a workspace.
10. **Seal the platform seam.** Move objc fork-safety + `DD_GPU_POOL` seeding behind
    `MacPlatform::prelaunch_env`; add stub `LinuxPlatform`; add the CI grep guardrail. *Gate:* full matrix +
    confirm bare Linux `cargo build` never compiles gtk/gpu-mac paths (default-members unchanged).

Reordering note: steps 4–6 (feature extraction) must precede 7–9 (root/GUI) so the GUI is rewritten against
the *final* feature/schema API, not twice.

---

## 7. Risks / open questions for the maintainer

1. **Two seams.** I keep `hl-jit::DeviceRequest` as the runtime-neutral seam and add `hl-ws::LaunchEffect`
   as the app-layer seam, bridged in `hl`. Alternative: push `LaunchEffect` down and delete `DeviceRequest`.
   Recommendation: keep both (hl-jit must not learn app vocabulary). **Confirm.**
2. **Opaque feature config (`BTreeMap<String,String>`)** keeps `hl-ws` truly feature-agnostic but moves
   validation to the feature (runtime parse, not compile-time). Acceptable, or do you want a typed
   registry (`Box<dyn Any>` per feature)? Recommendation: opaque string — it round-trips through the same
   `.conf` and matches today's spec strings.
3. **Is `gui`/display a feature or a core toggle?** I model it as `features["display"]` (a plugin), which is
   consistent but changes the persisted key. **Confirm** the display integration should be a plugin peer of
   vpn/cuda rather than a built-in.
4. **`docker_sock`** stays a core `Workspace` field (not a plugin) — it's runtime plumbing, not a
   composable feature. Agree?
5. **Two GUI binaries.** `dd-app` (container manager, hl-client) and `dd-term` (workspace terminal) both
   land in `hl`. Is the container manager staying, or converging into the workspace app? Affects how much of
   `ui/views/{containers,images,networks,volumes,system}` becomes `hl-ws-gui` primitive vs `hl`-local.
6. **Crate renames on the shared tree.** `hl-term→hl-ws-term`, `hl-cli→hl`, `hl-gui→hl-ws-gui` are
   `Cargo.toml`/`default-members`/path churn that will collide with any concurrent codex work; they must be
   scheduled as isolated, tree-quiet commits (memory `shared-main-tree-codex`).
7. **Persisted-data compat.** Plan reads legacy `vpn=`/`cuda=`/`gui=true` into the feature bag; confirm we
   also **write** the new `feature.<id> =` form (one-way upgrade on next save) vs. keeping old keys for a
   deprecation window.
8. **`LinuxPlatform` scope.** This phase only lands the seam + a stub; real linux-host support (state dir,
   service manager, no-objc launch) is a later phase. Confirm that's the intended boundary.
