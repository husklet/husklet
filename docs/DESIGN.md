# Design

Good architecture lets a capability be added, removed, or replaced without modifying unrelated code. This
requires enough abstraction to isolate real boundaries, but no abstraction without a concrete reason.

## Place logic by ownership

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

## Packages

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

## Domain crate shape

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

## Models: entities and values

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

## Library machinery inside a domain

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

## API surfaces

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

## Services

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

## Ports and adapters

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
adapters/jit.rs          `hl-jit` implementation
adapters/memory.rs       deterministic test implementation
```

Do not create a trait solely to rename one concrete type. Add a port for substitution, platform separation,
testing, or a stable architectural boundary. Keep traits small; split unrelated capabilities rather than
building a repository or service locator for the whole application.

The composition root supplies implementations:

```rust
let engine = Jit::open(engine_config)?;
let images = Registry::open(registry_config)?;
let workspaces = Workspaces::new(images, engine);
```

Replacing `Jit` with another engine changes construction in `husklet`, not workspace use cases or endpoints.
That is the practical test of the boundary.

## Review a placement

Before adding code, state its owner and reason:

| Logic | Place | Reason |
|---|---|---|
| Read bytes atomically from a path | `packages/hl-fs` | Generic filesystem mechanism |
| Parse a number from HTTP request state | `packages/hl-httpio` | Generic protocol extraction |
| Validate workspace memory limits | workspace `model` | Domain invariant |
| Render a generic currency input | GUI `lib/currency` | Transferable within GUI domains |
| Open a workspace from image + engine | workspace `service` | Multi-capability use case |
| Define what workspace execution requires | workspace `ports::Engine` | Stable replaceable boundary |
| Translate `hl-jit` calls into `Engine` | application adapter | Concrete integration |
| Decode `POST /workspaces` | `api/http` | Transport adapter |

A placement is wrong when moving or replacing one mechanism requires edits across unrelated models,
services, and endpoints. A placement is over-abstracted when its interface has no plausible second
implementation, test seam, platform boundary, or stable contract.
