# Project design rules

## Product and architecture

Husklet provides configurable workspaces. Opening a workspace enters its configured Linux image and
gives the user a terminal plus workspace services such as VPN configuration. Linux applications run
inside the emulated environment; an application such as `google-chrome` uses Wayland and the guest GL,
CUDA, or Vulkan libraries, which lower work into the neutral GPU IR. WebGPU executes that IR on the host.
Wayland surfaces are composed and presented through the host surface implementation, which is currently
macOS-only.

The signed `husklet` application is the product composition root. It owns the GUI and CLI product
surfaces, selects implementations, supplies shared libraries to containers, connects GPU and surface
services, and translates user intent into engine/container operations. Lower-level crates must not grow
product policy merely because Husklet currently happens to be their only caller.

The repository's physical layout is rooted under `src/`:

```text
src/
  engine/
    hl-jit/
    hl-jit-darwin/
  containers/
    hl-daemon/
    hl-client/
    hl-images/
  gpu/
    hl-gpu/
    hl-gpu-wgpu/
  surface/
    hl-compositor/
    hl-gl/
    hl-cuda/
    hl-vulkan/
  workspaces/
    hl-gui/
    hl-ws/
    hl-ws-term/
  packages/
    hl-log/
  apps/
    husklet/
```

Keep the `hl-` prefix on reusable Cargo packages. The signed product application is simply `husklet` and
must never be used as a dependency. Directories group crates by domain; they are not additional Rust
namespaces and do not justify repeating group names in crate APIs.

### Dependency direction

- `packages/` contains general-purpose foundations and may be depended on by every domain. It must not
  depend on product or domain crates.
- Domain groups (`engine/`, `containers/`, `gpu/`, `surface/`, and `workspaces/`) should be independently
  buildable and testable. Prefer their own domain models and narrow port traits over reaching into another
  group's concrete implementation.
- A domain may depend on another domain's stable contract when the relationship is inherent, but concrete
  backend selection and multi-domain orchestration belong in `apps/husklet`.
- `apps/husklet` may depend on all groups. No library crate may depend on `husklet`.
- Keep neutral protocols and wire models with the domain that owns them. Backends implement those contracts;
  callers do not copy the models or branch on backend-specific details.
- Cargo's root workspace lists every crate by its grouped path. Use workspace dependencies for shared
  third-party version alignment, while keeping each crate's feature set and platform dependencies local.
  `default-members` should remain the portable, headless subset; platform applications and heavy GPU/UI
  backends are built explicitly.

### Composition seams

Define traits at genuine architectural boundaries: engine execution, images/containers, GPU command sinks,
surface presentation, workspace persistence, terminals, and GUI event delivery. Traits describe one
capability with domain language and should not become a single application-wide service locator.

Configuration and events cross these seams as owned domain values. For example, `hl-gui` may render a VPN
field and emit a typed value-change event; `husklet` decides that this updates a workspace and forwards
the resulting engine configuration to `hl-jit`. The GUI must never import JIT code or set VPN state itself.

## GUI rules

`hl-gui` is a versatile, configuration- and component-driven presentation library. It defines reusable
windows, rows, buttons, forms, lists, settings panes, navigation, validation presentation, and typed user
events. It defines how supplied state is displayed and how user changes are reported, not what those
changes do.

- Components receive view data and emit events or invoke narrowly typed `on_change`/`on_submit` handlers.
- Settings panes are schemas/compositions supplied by the application. `hl-gui` can render a workspace VPN
  setting, but it cannot decide which settings exist, persist them, start a VPN, or call the engine.
- Components do not depend on containers, JIT, GPU backends, or product configuration. Put adapters from
  domain state to GUI view models in `husklet` unless the view model is genuinely generic.
- Keep toolkit-specific widgets behind GUI-owned adapters. Public component/configuration models should be
  testable without starting the native toolkit.
- Prefer composition of small components and typed event handlers over subclassing, global callbacks, or
  screens with embedded product logic.
- The GUI must support the same component model for workspace windows, terminals, settings, and surfaces;
  a new product feature should normally be assembled in `husklet` without modifying the generic GUI
  library.

## Runtime flow

```text
husklet configuration/UI
  -> workspace and container services
  -> emulated Linux process (`hl-jit` / `hl-jit-darwin`)
  -> Wayland + guest GL/CUDA/Vulkan libraries
  -> neutral `hl-gpu` IR
  -> host `hl-gpu-wgpu`
  -> `hl-compositor`
  -> macOS surface
```

This flow describes ownership, not permission for layers to reach through one another. Each arrow should be
a small explicit contract, and the Husklet application wires the concrete pieces together.

These rules describe the preferred shape of production code in this repository. Apply them when
adding code and improve nearby code when the change is safe and reviewable. They are defaults, not
an excuse to force an abstraction where the domain does not contain one.

## Model the domain

- Organize behavior around domain entities and cohesive services. Prefer noun-named types such as
  `Container`, `Containers`, `Layer`, `Leases`, and `Registry`.
- Put behavior that primarily uses one value on that value: prefer `descriptor.is_document()` over
  `is_document(&descriptor)`, and `layer.apply(root)` over `apply_layer(layer, root)`.
- Use associated constructors (`new`, `open`, `from_*`) and methods to make valid state and ownership
  obvious. Avoid orphan helper functions when a clear receiver exists.
- Name state with a precise noun. Use suffixes such as `Config`, `Request`, `Event`, or `Result` only
  when they distinguish genuinely different domain concepts. Avoid verb nouns such as `Launch` for
  persistent configuration; prefer a concrete noun such as `ProcessConfig`.
- Use plural service names for collections/managers when the shorter domain name is unambiguous:
  prefer `Leases` to `LeaseManager` and `Containers` to `ContainerService`.

## Traits and implementations

- Keep generic contracts near the root of their domain module. Put concrete implementations below
  them, for example `storage/{mod.rs,memory.rs,file.rs}`.
- A trait should have one responsibility and use short method names within that responsibility.
  Prefer separate `Containers` and `Logs` traits with `Logs::{read, append, remove}` over one broad
  repository trait with `logs`, `append_log`, and `remove_logs`.
- Do not create a trait merely to hide one concrete type. Introduce a seam for substitution,
  platform separation, testing, or a stable architectural boundary.
- Keep public wire models owned by the server/API crate. Clients reuse those exact types instead of
  copying them, so protocol changes are checked by the compiler.

## Files and modules

- Prefer short, single-word file and directory names. Names describe the entity or responsibility,
  not its implementation history.
- A module is already a namespace; do not repeat its name in every contained type. Prefer
  `rootfs::Reference`, `rootfs::View`, and `rootfs::Roots` to `rootfs::RootfsRef`,
  `rootfs::RootfsHandle`, and `rootfs::FsRootfsManager`.
- Choose a precise noun instead of keyword-like abbreviations (`Reference`, not `Ref`) and avoid
  generic suffixes such as `Manager`, `Helper`, `Util`, or `Impl`. A plural entity service usually
  communicates ownership better.
- Before renaming, search the full workspace and classify repeated prefixes as redundant namespace,
  necessary crate-root disambiguation, protocol-standard terminology, or genuinely distinct domain
  concepts. Do not shorten names mechanically when it makes re-exports ambiguous.
- Do not add a directory containing only a trivial `mod.rs`. Use a file until the module has enough
  cohesive children to justify a directory.
- Split a growing module by responsibility. HTTP servers should have an `api` domain with separate
  handler, middleware, and wire-model modules rather than one router file containing every endpoint.
- Library crates do not ship demonstration or redundant binaries. A binary exists only when it is a
  supported product surface with its own tests and packaging.
- Keep the repository workspace focused on this product; do not inherit workspace structure from a
  different project.

## Control flow and readability

- Keep the happy path shallow. Extract a named method when nested async loops/error handling cause
  excessive indentation.
- Prefer early returns and small cohesive methods over deeply nested closures and anonymous async
  blocks.
- Names should remove the need for comments that merely restate the code. Comments explain contracts,
  safety, compatibility, or non-obvious reasons.
- Reject unsupported meaningful configuration explicitly. Never deserialize and silently ignore a
  request that the runtime cannot honor; accepting harmless protocol default values is fine.

## Thin transport endpoints

- HTTP/RPC handlers are adapters. They extract transport inputs, invoke domain models or services,
  translate typed errors into protocol errors, and encode the response. Keep validation, parsing,
  persistence, orchestration, and formatting logic out of handlers.
- A free helper whose first meaningful argument is one project domain type is usually misplaced.
  Put the behavior on that receiver unless it genuinely combines peer entities or implements a
  low-level algorithm.
- Prefer `EnvVars::parse(values)` to a handler-local `parse_environment(values)` function.
- Prefer `progress.bytes()` to `progress_bytes(&progress)` when serialization belongs to the
  response model.
- Prefer `state.find_image(name).await` to `find_image(&state, name).await` when lookup belongs to
  the server state/service graph.
- Prefer standard conversion traits (`From`, `TryFrom`, `FromStr`) when the operation is a real type
  conversion. A named associated method is better when it communicates domain policy more clearly.
- Framework handler functions may remain free functions because the framework owns their extractor
  signature. Their bodies should still delegate meaningful work immediately to receivers.
- Before adding a standalone helper, classify it as one of: receiver behavior, associated
  construction/conversion, multi-entity operation, low-level algorithm, framework adapter, or test
  fixture. Only the last four normally remain free functions.

## Tests and changes

- Tests verify observable behavior, state transitions, wire compatibility, failure semantics, and
  performance where relevant. Do not test by reading source files or searching for implementation
  text.
- Keep tests in Rust or C. Prefer deterministic fixtures and golden outputs for compatibility cases.
- Make small coherent commits and run the narrowest relevant tests after each change, followed by
  workspace tests and strict linting at integration points.
- Preserve runtime compatibility and performance while refactoring. A cleaner shape is not complete
  until behavior is proven equivalent or intentionally improved.
