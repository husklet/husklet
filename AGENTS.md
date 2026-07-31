# Husklet instructions

These rules define the durable product, architecture, design, coding, and style standards.
Apply them to new code and improve nearby code when safe. Preserve behavior during refactors and prove
changes through observable tests. Do not force abstractions without a domain concept or boundary.

## Mission

Husklet provides isolated, reproducible Linux workspaces. Opening one enters its configured image with a
terminal and services such as networking and VPN.

Linux CLI, GUI, and GPU applications should run without guest-specific setup. Guest Wayland and
GL/CUDA/Vulkan work crosses explicit host boundaries, executes on host hardware, and appears as responsive,
native-quality windows.

The product composes replaceable engine, container, GPU, surface, workspace, and GUI capabilities. A
capability is complete when ordinary applications use it correctly, reliably, and at native-feeling
latency—without application workarounds, hidden stubs, or product policy in reusable packages.

## Architecture

`apps/husklet` is the composition root. It owns product configuration, GUI/CLI behavior, backend selection,
and cross-domain orchestration. No crate depends on it.

```text
src/
  apps/husklet/
  packages/{hl-fs,hl-log}/
  containers/{hl-container,hl-daemon,hl-client,hl-images}/
  gpu/{hl-gpu,hl-gpu-wgpu}/
  surface/{hl-compositor,hl-gl,hl-cuda,hl-vulkan}/
  workspaces/{hl-gui,hl-ws,hl-ws-term}/
../engine/pkgs/rust/ (`hl-engine`)
```

- Reusable packages use `hl-`; the product is `husklet` and is never a dependency.
- `packages` is general-purpose and depends on no domain.
- Domains own their models and remain independently buildable and testable.
- Cross-domain edges use small capability traits and owned domain values. Concrete wiring belongs in
  `husklet`.
- Protocol and wire types have one owner. Clients reuse them; backends implement them.
- Traits mark substitution, platform, test, or stable domain boundaries—not every concrete type.
- Platform code stays behind domain-owned adapters.

Runtime ownership:

```text
husklet -> workspace/container -> Linux engine -> Wayland + GL/CUDA/Vulkan
        -> GPU IR -> host GPU -> compositor -> host surface
```

### UI

- `hl-gui` owns generic visual primitives, layout, validation display, and toolkit adapters.
- `husklet` owns screens, settings schemas, domain view models, and feature composition.
- Components receive state and emit typed intent. They do not persist, orchestrate, or invoke services.
- Domain components compose generic components beside the product feature that owns them.
- Native toolkit types do not cross the GUI boundary.

#### Generic and feature components

Apply the same ownership test to native UI that frontend projects apply to reusable components:

- A generic component such as `Button`, `Input`, `Dialog`, `Modal`, `List`, or `Settings` understands
  presentation, interaction state, accessibility, and typed events. It contains no workspace, image,
  container, VPN, or product policy and belongs in `hl-gui`'s component namespace.
- A feature component such as a workspace picker, image chooser, removal confirmation, or terminal settings
  page composes generic components and carries domain meaning. It stays beside the Husklet page or feature
  that owns it.
- A domain-to-view model belongs with the feature or application adapter. Do not add toolkit methods to a
  domain entity merely to turn a helper into a method.

Use Rust modules to express the same shape as frontend `_components` and page-local components without
copying TypeScript naming conventions mechanically:

```text
hl-gui/src/
  component/
    button.rs
    input.rs
    dialog.rs
    dialog/
      confirm.rs

apps/husklet/src/
  workspace/
    page.rs
    component/
      picker.rs
      removal.rs
```

Begin with `component/dialog.rs`. Convert it to `component/dialog/{mod.rs,dialog.rs,confirm.rs}` only when
cohesive variants or implementations justify children. The root `Dialog` defines general state and behavior;
`Confirm` may compose or construct a `Dialog` when confirmation adds reusable semantics. A product-specific
`RemoveWorkspace` composes `Dialog` or `Confirm` in the workspace feature—it does not belong in `hl-gui`.

Do not equate every visual variation with a new component. Add a component when it has a stable concept,
state, interaction contract, accessibility behavior, or multiple cohesive operations. Keep one-off page
layout beside the page. Generic components emit intent; feature components and Husklet decide effects.

Cargo lists crates by grouped path. Shared dependency versions live at the workspace root; features and
platform dependencies stay local. Portable headless crates remain the default build set.

## Design

Good architecture lets a capability be added, removed, or replaced without modifying unrelated code. This
requires enough abstraction to isolate real boundaries, but no abstraction without a concrete reason.

### Place logic by ownership

Place code in the lowest layer that fully owns its meaning.

```text
application -> service -> api/model/lib -> packages
                         -> ports <- adapters
```

Ask these questions in order:

1. Is it independent of Husklet and its domains? Put it in `packages/`.
2. Is it meaningful only inside one domain? Put it in that domain crate.
3. Does it describe a domain value or its invariants? Put it in `model`.
4. Is it the supported way callers use the crate? Put it in `api` or re-export it from the crate root.
5. Does it combine models, libraries, and ports into a capability? Put it in `service`.
6. Is it reusable implementation machinery within this kind of domain? Put it in the domain's `lib`.
7. Does it cross an I/O, platform, process, or replaceable implementation boundary? Define a `port` and put
   the concrete mechanism in an adapter.

Do not classify code by convenience. `util`, `utils`, `core`, `common`, and `shared` hide ownership instead
of defining it.

### Packages

`src/packages/` contains transferable foundations: filesystem primitives, HTTP I/O, logging, codecs, and
other logic whose vocabulary does not belong to a product domain. A package must not import a domain crate.

```rust
// packages/hl-httpio
pub trait ParseFromState<S>: Sized {
    type Error;

    fn parse(state: &S) -> Result<Self, Self::Error>;
}
```

Parsing an integer from generic request state belongs to `hl-httpio::ParseFromState`; deciding that the
integer is a valid workspace CPU limit belongs to the workspace model. Mechanism is general; policy stays
with the domain.

Do not move code to `packages/` merely because two callers exist. Move it only when its types, errors, and
rules make sense without either caller.

### Domain crate shape

A domain crate may contain these roles when each is needed:

```text
domain/
  src/
    model/       domain entities and values
    api/         public and protocol-facing surfaces
      http/      HTTP wire models and endpoints
    lib/         transferable domain-kind machinery
    service/     use-case orchestration
    ports/       capability contracts
    adapters/    concrete port implementations
    lib.rs       intentional re-exports
```

These names are roles, not mandatory empty folders. Start with a file. Create a directory only when it has
cohesive children. A small crate can expose the same shape directly from `lib.rs`.

Dependencies point inward:

```text
adapters -> ports
service  -> model + lib + ports
api      -> model + service
model    -> packages
lib      -> packages
```

`model` does not depend on transport, adapters, or services. `service` does not depend on concrete adapters.
The application selects adapters and constructs services.

### Models: entities and values

Models express domain language, valid state, invariants, and behavior. Put behavior on the value it primarily
uses.

```rust
pub struct Log {
    id: LogId,
}

impl Log {
    pub fn id(&self) -> &LogId {
        &self.id
    }
}

pub struct Logs<L> {
    storage: L,
}

impl<L: LogStorage> Logs<L> {
    pub fn read(&self, id: &LogId) -> Result<Vec<Record>, L::Error> {
        self.storage.read(id)
    }
}
```

`Log` owns one log's identity and invariants. `Logs` owns operations over the collection. Prefer these names
to `LogData`, `LogManager`, `LogService`, `read_log`, or a raw `Vec<Log>` passed between helpers.

Use associated constructors to make valid state and ownership explicit:

```rust
let reference = ImageReference::parse(input)?;
let workspace = Workspace::from_config(config)?;
let surface = Surface::new(id, size)?;
```

Use `From`, `TryFrom`, and `FromStr` for policy-free conversions. Use named constructors when conversion
applies domain policy.

Before adding or preserving a string, integer, or boolean that represents state, search its assignments and
comparisons for a finite vocabulary. Model a closed set as an enum and keep serialization at the wire or
storage boundary. For example, repeated `"Preparing"`, `"Pushing"`, and `"Pushed"` values should prompt a
`PushStatus` enum review. Preserve unknown protocol values explicitly when forward compatibility requires it.
Do not force open-ended messages, user text, identifiers, or extensible third-party values into closed enums;
use a precise newtype when they need identity or validation without a finite variant set.

### Compose specialized entities from a shared base

When multiple types repeat the same identity and state, extract that shared basis into one entity. A cluster
of three or more identical fields is a strong review signal, not a mechanical threshold: shared invariants,
lifecycle, and meaning decide whether the fields form a real entity. Do not let parallel structs drift by
copying fields, validation, or behavior.

Keep the base and its specialized forms in one precise namespace. Rust has no struct inheritance; represent
specialization through composition. The specialized type owns the base entity and adds only the state that
distinguishes it:

```rust
mod image {
    pub struct Image {
        reference: Reference,
        rootfs: Rootfs,
        arch: Arch,
    }

    pub struct Discovered {
        image: Image,
        source: Source,
    }

    impl Discovered {
        pub fn from_image(image: Image, source: Source) -> Self {
            Self { image, source }
        }

        pub fn image(&self) -> &Image {
            &self.image
        }

        pub fn into_image(self) -> Image {
            self.image
        }
    }
}
```

Prefer `image::Image` and `image::Discovered` to unrelated `Image` and `DiscoveredImage` structs that both
declare `reference`, `rootfs`, and `arch`. Put shared behavior on `Image`; put discovery-only behavior on
`Discovered`. Expose deliberate accessors or conversions rather than `Deref`, field forwarding, or a trait
that merely imitates inheritance.

Do not extract a base merely because fields share Rust types or names. Two values may use `name`, `path`, and
`id` with different semantics. Extract only when callers treat the fields as the same domain object and the
specialized value has an actual “is this entity plus more state” relationship. If specialized states are
mutually exclusive lifecycle stages, also consider an enum containing the shared entity instead of several
wrappers.

### Classify emerging concepts

Do not create a type only to turn one small helper into a method. First decide whether the function is a
complete low-level algorithm, belongs on an existing receiver, or is the first evidence of a missing concept.

```rust
fn clean(value: &str) -> String;
```

`Path::clean()` alone does not justify a new `Path` wrapper. If nearby behavior accumulates around the same
value—such as cleaning, removing when present, and rejecting non-ASCII input—the cluster reveals a path
entity and should move together into the filesystem package. Judge the cluster by shared invariants and
ownership, not function count alone. Do not build a wrapper from unrelated operations that merely accept the
same primitive type.

This provisional workflow applies only to the free-function lint for standalone functions with one or two
arguments. It does not suppress environment, single-use, nesting, or other lint rules. When the correct owner
is visible but the concept is not mature enough to extract, mark that one- or two-argument function with a
temporary classification:

```rust
#[hl_design::classify(root = fs)]
fn clean_path(value: &str) -> String;

#[hl_design::classify(domain = gpu)]
fn normalize_binding(value: u32) -> u32;

#[hl_design::classify(pkg)]
fn output_scale(value: f64) -> i32;

#[hl_design::classify(struct = Path)]
fn remove_non_ascii(value: &str) -> String;
```

Classifications are review queues, not permanent suppressions:

- `root` identifies general functionality that should eventually move to `packages/`. Use precise standard
  domains such as `fs`, `io`, `encoding`, or `validation`.
- `domain` identifies functionality accumulating inside a repository domain such as `gpu` or `surface`.
- `pkg` identifies behavior owned by the package containing the function but not yet supported by a mature
  entity. The linter derives the nearest Cargo package; never repeat its name in the annotation.
- `struct` identifies behavior accumulating around a potential entity that does not yet justify extraction.

Unclassified findings are lint errors and belong in the flat `lint/errors/` queue. Classified findings belong
in the flat `lint/check/` queue. Case names use
`<unix_timestamp>_<domain>_<package>_<function>.md` with snake_case components. Agents periodically review
`check`, combine related functions into entities or
packages, and remove the annotation after refactoring. A finding disappears only when its design issue is
resolved; classification merely moves it out of the untriaged queue.

#### Subagent protocol for design lint cases

Resolving `lint/errors/` starts by spawning subagents. Give each subagent a small, disjoint batch of related
case files from one domain and preferably one package or cohesive source area. A batch should contain only a
few cases that can be understood and tested together; do not assign repository-wide queues or mix unrelated
GPU, surface, container, workspace, and application cases. Never let agents edit overlapping source files.

Every assignment must explicitly require the subagent, before editing, to read all of `AGENTS.md`, all of
`lint/examples/positive.md`, and all of `lint/examples/negative.md`. The subagent must then read each assigned
case, current source function, reported uses, enclosing callers, sibling functions, relevant models, owning
crate manifest, and nearby tests. A generated case is evidence, not source of truth and may be stale; verify
every location against current Rust source.

Reading the examples is a mandatory gate, not a suggestion or a request to skim. The subagent must read both
files from beginning to EOF, including every code block and rejection reason, and explicitly confirm in its
report: `Read positive.md in full: yes` and `Read negative.md in full: yes`. A summary, search result, partial
read, inherited conversation context, or another agent's interpretation does not satisfy this requirement.
The manager must reject work lacking both confirmations, even when its tests pass.

Subagents are implementors, not suppression approvers. Their primary task is to refactor the assigned cases
into the correct entities, collections, traits, standard conversions, modules, or intentional inline code,
then run focused tests. If a safe final design cannot yet be justified, the subagent must leave production
source unannotated and report the proposed classification, exact evidence, alternative owners considered,
and what evidence is missing. A subagent must not add `#[hl_design::classify(...)]`,
`#[hl_design::naming(...)]`, lint allowances, or dependencies whose only purpose is suppression unless the
manager explicitly approved that exact use in advance.

The manager reviews every subagent diff against the source, both example documents, domain boundaries, and
test evidence. Accept a refactor only when behavior and ownership are proven. When refactoring is genuinely
premature, the manager validates the proposed scope and personally applies the narrow temporary macro. The
manager never rubber-stamps classifications, mass-applies attributes, or uses macros to make the queue empty.
Classification means “review later,” not “resolved.”

Append a pattern to `positive.md` only after user approval; append a rejected proposal to `negative.md` only
when the user classifies it as negative. Do not infer either decision from silence or “next.”

For each `lint/errors/` case:

1. Identify what the function means, not merely what its name says. Record its input concept, invariants,
   side effects, dependencies, and callers.
2. Search the package and domain for functions operating on the same concept. A shared primitive argument is
   evidence only; cohesive rules and invariants establish an entity.
   Also search repeated primitive values for a closed state set that should become an enum.
3. Prefer, in order: behavior on an existing receiver; an associated constructor or conversion; inlining a
   single-use implementation; a free multi-entity operation or low-level algorithm; a provisional
   classification when the owner is visible but extraction is premature.
4. Do not create a wrapper for one helper. Propose a new type only when several cohesive operations or a
   meaningful invariant justify it.
5. If a final design cannot be justified, recommend the narrowest truthful classification: `root` for
   project-independent foundations, `domain` for behavior shared within a repository domain, `pkg` for the
   current Cargo package, or `struct` for an emerging entity. `pkg` never takes a name; Cargo ownership is
   derived automatically. Do not apply the macro; return the recommendation to the manager.
6. Refactor obvious cases instead of recommending annotation. Never add an exception merely because a
   refactor crosses several files or requires tests.
7. Report each case as `refactored`, `classification proposed`, or `blocked`, with its owner, API, reasoning,
   files changed, and tests. Leave generated approval fields unchecked unless the user explicitly delegates
   approval.

For each `lint/check/` case:

1. Review cases as a group by classification; the purpose of `check` is to detect when enough behavior has
   accumulated to reveal its final owner.
2. For `root`, verify the API is independent of Husklet, then extract it into a precise package. For
   `domain`, find the domain model or domain library that owns the cluster. For `pkg`, move behavior onto an
   existing package entity or precise module. For `struct`, verify identity/invariants and extract a newtype
   only when the cluster is cohesive.
3. Reclassify when evidence points to a narrower or different owner. Keeping an old classification because it
   already passes the lint is not acceptable.
4. Remove the annotation when behavior is inlined, moved onto a receiver, or extracted behind its final API.
   A classified free function is unfinished design.
5. Leave the case in `check` when evidence remains insufficient; document what additional behavior would
   justify extraction. Do not manufacture abstractions to empty the queue.

After each subagent batch, the manager inspects the diff and runs or verifies the narrow tests for affected
crates. After accepted batches are integrated, run `cargo test -p hl-design-lint` and `make lint-cases`.
Confirm resolved cases disappear, manager-approved classifications move to `lint/check/`, unclassified cases
remain in `lint/errors/`, both queues stay flat, and no unrelated case is lost. Never mass-annotate, classify
from a function name alone, edit generated code snippets instead of Rust source, or weaken the lint to make
the build pass.

### Library machinery inside a domain

`lib` contains machinery reusable by another implementation of the same kind of domain, but not general
enough for `packages/`. It must not contain product policy or become a dumping ground.

```text
workspaces/hl-gui/src/
  model/
  api/
  lib/
    currency/
      amount.rs
      format.rs
  service/
```

GUI currency input and formatting can transfer to another GUI library, so `gui::lib::currency` is reasonable.
Workspace billing policy is not: it belongs to the product or workspace domain. If currency handling later
becomes useful outside GUI systems and loses all GUI vocabulary, it can move to `packages/`.

Prefer a precise namespace over a broad `lib` file. `lib/currency`, `lib/layout`, or `lib/text` communicates
ownership; `lib/helpers.rs` does not.

### API surfaces

`api` is every supported way another party interacts with a crate. Rust methods, events, commands, and wire
protocols are all API surfaces. The crate root re-exports the small, stable Rust surface; implementation
details remain private.

```rust
// lib.rs
pub use api::{CreateWorkspace, WorkspaceEvent};
pub use model::{Workspace, WorkspaceId};
pub use service::Workspaces;
```

Protocol APIs live below their protocol:

```text
api/
  http/
    request.rs
    response.rs
    workspace.rs
  rpc/
    command.rs
    event.rs
```

Wire models have one owner. A client imports the server/domain-owned request and response types instead of
copying them. Protocol handlers are thin adapters:

```rust
pub async fn create(
    State(workspaces): State<Arc<Workspaces>>,
    Json(request): Json<CreateWorkspace>,
) -> Result<Json<WorkspaceView>, HttpError> {
    let workspace = workspaces.create(request.try_into()?).await?;
    Ok(Json(WorkspaceView::from(workspace)))
}
```

Extraction and response encoding belong to HTTP. Validation of workspace rules belongs to the model;
orchestration belongs to `Workspaces`.

Framework handlers may remain free functions because the framework owns their extractor signature. For
example, `info(State(state): State<DockerState>)` is an Axum adapter, not misplaced `DockerState` behavior.
Do not move a handler onto state mechanically. Keep its body limited to extraction, thin response
orchestration, error translation, and encoding; reusable domain behavior still belongs to models or services.
Mark a reviewed handler with `#[hl_design::adapter]`. The marker is valid only with a recognized framework
extractor signature and is not a general free-function suppression.

### Services

A service implements a use case by composing models, domain libraries, and ports. It is the layer called by
an endpoint or application. A service should speak in domain nouns and typed operations, not expose its
dependencies as a generic state bag.

```rust
pub struct Workspaces<I, E> {
    images: I,
    engine: E,
}

impl<I: Images, E: Engine> Workspaces<I, E> {
    pub async fn open(&self, config: WorkspaceConfig) -> Result<Workspace, OpenError> {
        let config = config.validate()?;
        let image = self.images.resolve(config.image()).await?;
        let process = self.engine.start(ProcessConfig::from_workspace(&config, image)).await?;
        Ok(Workspace::opened(config, process))
    }
}
```

The service owns the workflow, not filesystem or HTTP mechanics. Keep an endpoint slim enough that the same
service can be called from HTTP, CLI, GUI, or tests.

Not every entity needs a service. An endpoint may call a model directly when the operation is local and
pure. Introduce a service when a use case coordinates multiple peers, I/O capabilities, or a transaction.

### Ports and adapters

A port is a narrow domain-owned trait for a capability that may vary by platform, process, backend, or test.
The domain names what it needs; an adapter translates a concrete mechanism into that contract.

```rust
pub trait Engine {
    type Error;

    async fn start(&self, config: ProcessConfig) -> Result<Process, Self::Error>;
    async fn stop(&self, id: &ProcessId) -> Result<(), Self::Error>;
}

pub trait Images {
    type Error;

    async fn resolve(&self, reference: &ImageReference) -> Result<Image, Self::Error>;
}
```

```text
ports/engine.rs          domain requirement
adapters/container.rs    `hl-container` implementation backed by `hl-engine`
adapters/memory.rs       deterministic test implementation
```

Do not create a trait solely to rename one concrete type. Add a port for substitution, platform separation,
testing, or a stable architectural boundary. Keep traits small; split unrelated capabilities rather than
building a repository or service locator for the whole application.

The composition root supplies implementations:

```rust
let engine = Containers::builder(container_config).build().await?;
let images = Registry::open(registry_config)?;
let workspaces = Workspaces::new(images, engine);
```

Composition ownership does not make `Application` the owner of every workflow. Keep it thin: construct and
expose cohesive domain services, then delegate behavior to them. Do not put `show_workspaces`,
`handle_workspace`, `open_terminal`, `remove_workspace`, and every other feature transition on one
application type merely because it can reach every dependency. That creates a service locator and a god
object.

Prefer navigation through the owning capability:

```rust
application.workspaces().show();
application.workspaces().get(id)?.show();
application.workspaces().get(id)?.terminal().open();
```

Here the application-level workspace facade may combine the workspace domain entity with its product view
and navigation behavior. The lower-level workspace model remains independent of GUI types. `Application`
wires the facade; `Workspaces` owns collection workflows; the selected application workspace owns
single-workspace behavior.

`Application` may register handlers for workspace events because connecting capabilities is composition-root
work. Keep registration declarative and handler bodies thin: translate a cross-capability event when needed,
then delegate to the owner.

```rust
application.workspaces().on(Event::Open, {
    let workspaces = application.workspaces();
    move |id| workspaces.get(id)?.show()
});

application.workspaces().on(Event::Settings, {
    let settings = application.settings();
    move |_| settings.show()
});
```

Do not interpret permission to register events as permission to implement workspace state transitions,
rendering, persistence, or terminal workflows on `Application`. Registration says who receives intent; the
receiving capability still owns what that intent means.

Replacing the container engine changes construction in `husklet`, not workspace use cases or endpoints.
That is the practical test of the boundary.

### Review a placement

Before adding code, state its owner and reason:

| Logic | Place | Reason |
|---|---|---|
| Read bytes atomically from a path | `packages/hl-fs` | Generic filesystem mechanism |
| Parse a number from HTTP request state | `packages/hl-httpio` | Generic protocol extraction |
| Validate workspace memory limits | workspace `model` | Domain invariant |
| Render a generic currency input | GUI `lib/currency` | Transferable within GUI domains |
| Open a workspace from image + engine | workspace `service` | Multi-capability use case |
| Define what workspace execution requires | workspace `ports::Engine` | Stable replaceable boundary |
| Translate `hl-container` calls into `Engine` | application adapter | Concrete integration |
| Decode `POST /workspaces` | `api/http` | Transport adapter |

A placement is wrong when moving or replacing one mechanism requires edits across unrelated models,
services, and endpoints. A placement is over-abstracted when its interface has no plausible second
implementation, test seam, platform boundary, or stable contract.
## Coding

### Types and ownership

- Start with the smallest meaningful entity; compose entities into collections and services.
- Put behavior on the value it primarily uses. Separate deterministic logic from I/O.
- Make invalid states unrepresentable with constructors, newtypes, and enums—not strings or related booleans.
- Borrow for observation; transfer ownership for storage or transformation. Clone only when ownership
  requires it.
- Accept slices and borrowed strings when ownership is unnecessary. Use the shortest useful lifetime.
- Choose `Box`, `Rc`, `Arc`, locks, atomics, and channels for their ownership semantics—not by habit.
- Match numeric representation to the domain. Use integers or fixed precision for exact quantities; floats
  represent approximate values.
- Use checked arithmetic where overflow is invalid and saturating arithmetic where clamping is the contract.
- Reserve collection capacity when a meaningful bound is known; do not allocate or collect intermediates
  without need.

### Boundaries and errors

- Give each trait one capability. Prefer standard traits and concrete code to ceremonial abstractions.
- Keep contracts at the domain root and external mechanisms in adapters.
- Cross boundaries with owned typed values. Do not copy models, expose globals, or branch on backends.
- Return typed `Result` errors from libraries; add actionable context at application and transport
  boundaries. Do not discard failures or panic for recoverable input.
- Do not use `unwrap` or `expect` in library paths unless a local invariant proves failure impossible and the
  invariant is evident or documented.
- Use `?` for propagation. Handle intentionally ignored failures explicitly and explain why they are safe.
- Validate untrusted input at its boundary. Parameterize external query/protocol values; never build commands
  by interpolation.
- Isolate `unsafe`; every block states the invariant that makes it sound.
- Minimize unsafe scope. FFI and byte-layout types use an explicit representation; avoid `transmute` when a
  typed conversion exists.
- Use builders only when construction has many optional or order-independent choices; otherwise use a
  constructor.

### Concurrency

- Introduce concurrency only for real parallelism or latency. Do not block an async executor; define task
  ownership, cancellation, and shutdown. Prefer messages or atomics before shared locks.
- Move blocking or CPU-heavy work off async executors. Bound spawned work and preserve result/error handling.
- Avoid holding locks across `.await`; reuse long-lived clients and resource pools.

### Delivery

- Keep transport handlers thin: decode, call domain behavior, map errors, encode.
- Reject unsupported meaningful input. Never accept configuration the runtime ignores.
- Keep libraries free of product policy and demonstration binaries.
- Until Husklet has a public release, internal persisted formats, unpublished Rust APIs, and unpublished
  protocols have no compatibility contract. Change them directly, delete obsolete readers and migrations,
  and fail clearly on stale data instead of carrying fallbacks. Do not retain permanent categories for
  obsolete internal implementations. This does not permit dropping compatibility with external standards
  Husklet implements, such as Docker, OCI, Wayland, CUDA, Vulkan, or host platform APIs; name those paths by
  the exact protocol version, extension, or deprecated symbol they support.
- Treat external executables, command-line flags, filesystem ownership, permission bits, and host utilities as
  platform adapters. A well-shaped entity around `Command` improves ownership but does not make the mechanism
  portable. Keep the domain contract platform-neutral, select the adapter at composition, and test capability
  behavior on every supported host rather than assuming executable names or GNU/BSD flag compatibility.
- Write a failing behavioral test when fixing a defect or adding testable behavior. Test exact outcomes, edge
  cases, errors, public workflows, and invariants; use property tests when examples cannot cover the state
  space.
- Keep repository tests unit-scoped and beside their owning source. Tests that launch processes, compile guest
  programs, require host applications or hardware, automate platform UI, or cross package/runtime boundaries
  belong in `../e2e`, where they consume built artifacts and public APIs.
- Keep tests deterministic, independent, and responsible for their resources. Expected panics name the
  expected condition; production code does not panic merely to simplify a test.
- Refactor incrementally; prove behavior, protocols, failures, and relevant performance.

## Style

- Every line and abstraction has maintenance cost; delete code that carries no behavior or contract.
- Use precise nouns. Collections/services may be plural: `Container`, `Containers`, `Layer`, `Registry`.
- Use `Config`, `Request`, `Event`, or `Result` only when they distinguish domain values.
- Name event and state types as nouns, not verbs, commands, adjectives, or past participles. Prefer `Change`,
  `Snapshot`, `Selection`, or `WorkspacesEvent` to `Changed`, `Selected`, or `Updated`. Variants may describe
  occurrences such as `Event::Removed(id)` because the enum supplies the noun identity. Choose the noun from
  what the value contains and guarantees; do not append `Event` merely to rescue an otherwise vague name.
- Avoid `Manager`, `Helper`, `Util`, `Impl`, vague abbreviations, and repeated module prefixes.
- A trait or type is already a namespace. Method names must not repeat the receiver name as a prefix or
  suffix: prefer `Directory::create`, `Archive::extract`, and `Workspace::remove` to
  `Directory::create_directory`, `Archive::archive_extract`, or `Workspace::remove_workspace`. Retain the
  repeated word only when it names a genuinely different domain concept rather than the receiver itself.
- Do not create catch-all modules or directories such as `core`, `common`, `shared`, `util(s)`, `helper(s)`,
  or `misc`. Name code by its entity, capability, algorithm, or external mechanism. Reuse does not define
  ownership.
- Treat a long type name as a namespace or modeling smell before abbreviating it.
- Prefer short single-word module and file names.
- A file remains a file until cohesive child modules justify a directory.
- Split modules by responsibility; mirror equivalent domain shapes where useful.
- Keep every Rust production and test file at or below 500 lines. Split by cohesive entities, adapters, or
  test behaviors; numbered fragments, `include!`, and oversized test monoliths do not create boundaries.
- Prefer receiver methods and standard conversions to orphan helpers. Extract a one-use function only when it
  names construction, isolates testable logic, or clarifies a real operation.
- Do not write getters that merely return a field unchanged. Public data models may expose fields directly
  when mutation cannot violate an invariant. Add a method when it validates, computes, normalizes, controls
  mutation, hides representation that callers must not depend on, or fulfills a stable trait contract.
  Hypothetical future encapsulation does not justify present boilerplate.
- Prefer standard Rust traits when they express the complete contract: `Display` for canonical user-facing
  text, `FromStr` for validated textual construction, `From`/`TryFrom` for conversions, and iterator traits
  for collection behavior. Do not create `format_*`, `parse_*`, `to_*`, or collection traversal helpers that
  duplicate a well-known trait. Implement a named method instead when multiple policies exist or the
  operation needs arguments that the standard trait cannot express.
- Keep the happy path shallow with early returns and small named operations.
- Follow Rust naming conventions: types and traits use `UpperCamelCase`, values and modules `snake_case`,
  constants `SCREAMING_SNAKE_CASE`.
- Derive or implement `Debug` for inspectable domain values. Derive other traits only when their semantics
  are correct.
- Use type aliases for readability, not domain distinction; use newtypes when values must not mix.
- Do not wrap a primitive or standard collection merely to make it look domain-driven. A newtype must enforce
  an invariant, prevent meaningful value mixing, own cohesive behavior, or provide a stable boundary. Prefer
  `Vec<String>` directly when a wrapper such as `Shell(Vec<String>)` would only forward vector operations.
- Prefer exhaustive matches for domain state. A wildcard must mean all remaining variants by contract.
- Comments explain contracts, safety, compatibility, or non-obvious reasons; names explain mechanics.
- Keep public APIs minimal. Rustdoc public contracts, invariants, errors, panics, safety, and non-obvious use;
  do not document the obvious to satisfy a quota.
- Keep lint suppressions local and justified. Do not hide warnings at crate or workspace scope for convenience.
- Search all callers before renaming. Do not shorten names when re-exports become ambiguous.

# Comments

Avoid excessive comments, short comments are fine, usually handy to tell what structure should do.
Everytime you add new functionality ask if domain/folder is right what is correct approach to structure
the api so its DX friendly.

# Running a fleet

Keep roughly six agents busy at all times. An idle agent is wasted capacity; a manager waiting on one agent
while five sit finished is the common failure.

## Keep agents alive; enqueue, do not respawn

**Send follow-up work to an existing agent rather than spawning a new one.** A finished agent still holds
everything it learned: the traces it captured, the theories it discarded and why, which files it already read,
which measurements were provenance-clean. A fresh agent starts blind and rebuilds that context from scratch,
which costs both tokens and wall-clock, and it rebuilds it *imperfectly* — it will re-chase leads the previous
agent already eliminated.

Six agents each holding half a million tokens of accumulated context is the desired state, not a problem to
manage. Prefer a long-lived agent with a deep context window over a short-lived one with a clean slate.

Spawn a new agent only when the work is genuinely a new area with no useful overlap, or when file ownership
would collide with what a live agent is holding.

A corollary: when an agent reports, reply to it. Even "nothing more for now, stand by" is better than letting
it lapse, because the next question in that area should go to the agent that already has the answers.

## Ownership and collisions

Partition by file ownership and state it explicitly in every assignment: what the agent owns, and which paths
other agents are live in. Two agents editing one file will silently clobber each other, and the manager will
commit the wreckage. Assign separate workspaces too — two agents on one workspace will `pkill` each other's
domain workers and invalidate each other's runs.

Tell each agent what the others are doing in the same crate. An agent that knows a neighbour owns
`service/frame/` will report a defect there instead of fixing it, which is the outcome you want.

## What to tell every agent

- The bundle hashes, and to **read them itself** rather than trusting the message. Bundles move.
- Not to rebuild or hot-swap the app bundle; the manager sequences builds from committed source in an
  isolated worktree, because a moving source tree makes the bundler refuse and mixed provenance makes every
  number worthless.
- To report a clean negative plainly. Killing a theory is a result. An agent that discards its own
  well-developed hypothesis on provenance grounds has done the job correctly.
- That a number without provenance is worse than no number.

## Bind a measurement to its target

Three wrong conclusions in one session came from measuring a real thing about the wrong target: the
window server listed *after* the probe exited, the *worker* pid sampled instead of its `__compositor`
child, and clicks aimed at a *different process's* windows. Each instrument was working; the binding was
wrong.

Bind every measurement to the process under test and to identifiers that process reported — a compositor
pid you resolved, a window number its own log emitted, a bundle hash you read yourself. Never to a name
like "Husklet", never to whatever happens to be on screen, never to a hash quoted in a message.

Ask a process where its output goes rather than assuming the conventional path:
`lsof -p $(pgrep -f 'worker domain <ws>$') | grep '\.log'`. Two workers racing at startup can leave the
winner writing somewhere else entirely, and "the log was empty" is only evidence once you have confirmed
you are reading the log that process writes.

`strings` cannot verify a diagnostic whose level or values are runtime `{}` substitutions — the message
never appears contiguously in the binary. Check a distinctive literal tail instead.

Selecting a different GL inside a workspace is harder than it looks, for reasons that are easy to
misdiagnose. Caller environment **is** passed through — that was measured — but the graphics device
prepends its own driver directory to `LD_LIBRARY_PATH`, so the driver search finds Husklet first, and
Mesa's selection variables (`GALLIUM_DRIVER`, `LIBGL_DRIVERS_PATH`, `LIBGL_ALWAYS_SOFTWARE`,
`__EGL_VENDOR_LIBRARY_FILENAMES`) reach the process and do nothing, because Husklet's shim never
implemented them. A harness that sets them and believes it selected another driver is comparing Husklet
with itself. Prove which driver you got — `dladdr` on an entry point, plus `GL_RENDERER` — rather than
assuming a variable took effect. Note `env -i` selects correctly and strips `HL_GPU_EXEC`, after which the
driver links shaders and returns out-of-memory on everything, which reads as catastrophic regression.

Design a case so the wrong answer names its own cause. Uploading each mipmap level as a distinct flat
colour made the level actually sampled fall straight out of the pixels, which turned a vague "mipmapped
draws go flat" into "always samples the smallest level, even under a non-mipmap filter" in a single run.

Isolate at the finest granularity the work allows. Moving a differential from one process per family to
one per expression turned two apparent defects into collateral from an earlier wedge — they agreed once
isolated — and let three silent-failure triggers each name themselves.

When a test's comment states a specification requirement, check the specification rather than the comment.
Two tests were found asserting the opposite of the spec **in prose**, which makes them read as
authoritative to anyone auditing them; one claimed that a successful call must not clear a pending error,
and was green throughout a period when that bug blocked an entire conformance suite.

Quote a hash, a pid or a measurement only in the same message as the command output that produced it,
and quote a figure you were given verbatim rather than from memory. An agent once stated a bundle hash
before reading it and happened to be right — which is worse than being wrong, because it launders a guess
into a confirmation that nothing downstream can distinguish from a real one.

The checks we build into a harness are usually the checks the report needs too. That same agent's
harness refuses a run whose renderer is not the one it asked for, and it did not apply that standard to
its own prose.

Check that two hypotheses predict **different** observables before running the test meant to separate
them. A discriminator was once run where both candidate explanations predicted the same image; it returned
a confident-looking result that meant nothing, and the flaw surfaced only when the predictions were
re-derived afterwards.

A differential finds *where* something is wrong and is silent on *why*. Three times in one session a real
disagreement had a root cause different from its obvious reading — once the driver under test was the more
correct side, once forty differences were the harness's own rounding ties, and once a depth defect took two
wrong framings before the third was right. Budget for that gap; a disagreement is a location, not a
diagnosis.

"Not in my crate" and "not mine to fix" are different questions. An investigation correctly established a
defect was downstream and stopped there — downstream included a vendored dependency it could have read and
edited, and the fault was two lines inside it.

Prefer an instrument that reports a positive count over one that reports absence, and when an
instrument reports absence, verify it can detect presence at all. Four separate zeros in one session were
artifacts rather than facts. **A positive count is only trustworthy with a denominator** — `count=100` is
not a measurement, `count=100 over_ms=381159` is, and the difference between a hundred failures across a
hundred frames and a hundred across six minutes demands opposite responses.

Establish a baseline from a known-good subject before concluding anything from a suspect one. A client
that works showed the identical failure signature to the one under investigation, which is what proved the
signature was normal.

Two corollaries worth stating because both cost time here. A latched diagnostic cannot distinguish
"happened once at startup" from "happening every frame" — carry an occurrence count. And an instrument
that is self-consistent and produces plausible numbers is not thereby correct: a probe that sent
`wl_surface.destroy` where it meant `attach` reported a stable, believable zero for hours. Check an
instrument against something outside itself before trusting a negative from it.

## Manager discipline

Stage commits by file, never `git add -A` across a shared tree — you will sweep up another agent's
uncommitted work and mis-attribute it. Do not install a bundle while an agent is mid-measurement. When you
relay a finding between agents, mark clearly what was measured and what was inferred; a hypothesis passed on
as fact wastes the next agent's whole session.

# Work

keep your work in ../hl-work folder, for each expriment keep one folder or share the folder but concentrate all 
builds and tmp and other things in one directory. 

Keep eye on disk usage, and system, and dangerous sys opperations.

If you run in orb (ususally will do) or any other vm, check for `mac` command to execute on host.
Do not spread files randomly across the system. /tmp folder this not shared between vm and host.
Use ../hl-work to provide shared artifacts.
