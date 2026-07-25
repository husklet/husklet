# Negative design examples

Rejected transformations and the specific reason they should not guide subagents.

## Do not extract trivial conditional forwarding

Rejected while considering the `human_status` case:

```rust
fn timestamp_or(timestamp: i64, fallback: i64) -> i64 {
    if timestamp > 0 {
        timestamp
    } else {
        fallback
    }
}
```

This function does not create a concept, enforce a meaningful invariant, improve reuse, or isolate a
substantial algorithm. It merely names one conditional and forces the reader to jump away from the policy
that needs it. Keep the conditional at its use site, or make timestamp validity part of a real timestamp/state
model when enough cohesive behavior exists. Do not extract tiny functions merely to shorten another function.

## Do not turn the application composition root into a feature controller

```rust
impl Application {
    fn show_workspaces(&self) {
        let page = workspace::Page::new(
            self.workspaces
                .all()
                .map(WorkspaceView::from)
                .collect(),
        );

        self.present(page.view(), |application, event| {
            application.handle_workspace(event);
        });
    }

    fn handle_workspace(&self, event: workspace::Event) {
        match event {
            workspace::Event::Open(id) => self.open_terminal(id),
            workspace::Event::Create => self.create_workspace(),
            workspace::Event::Settings => self.open_settings(),
            workspace::Event::Remove(id) => self.remove_workspace(id),
            workspace::Event::Refresh => self.show_workspaces(),
        }
    }
}
```

This mixes application composition with workspace presentation, navigation, and workflows. Access to every
dependency does not make `Application` the correct receiver. It grows into a god object and leaves the
workspace capability as passive storage.

Delegate through the owning collection and entity instead:

```rust
application.workspaces().show();
application.workspaces().get(id)?.show();
```

`Application` wires and exposes the capability. `Workspaces` owns collection behavior; the selected
application workspace owns behavior for one workspace. Keep GUI behavior on an application-level workspace
facade rather than contaminating the lower-level workspace domain model with toolkit types.

Registering workspace handlers at the application root is valid composition:

```rust
application.workspaces().on(Event::Open, {
    let workspaces = application.workspaces();
    move |id| workspaces.get(id)?.show()
});
```

The negative pattern is putting the resulting workspace workflow on `Application`, not registering the
handler there. A root handler should translate or route intent and immediately delegate to its owner.

## Do not name a type as a past-tense condition

```rust
pub struct Changed {
    pub workspaces: Vec<WorkspaceView>,
}
```

`Changed` describes something that happened; it does not name the value. Structs should be precise nouns.
Name this from its actual contract, for example `WorkspaceSnapshot`, `WorkspaceChange`, or
`WorkspacesEvent`. If it is an event enum, past-tense occurrences belong in variants under a noun type:

```rust
pub enum WorkspacesEvent {
    Removed(WorkspaceId),
    Added(WorkspaceId),
}
```

Do not add an `Event` suffix mechanically. First decide whether the value is a snapshot, delta, selection,
command, or event, then use that noun.

## Do not add accessors that only return fields

```rust
impl Metadata {
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }

    pub fn options(&self) -> &BTreeMap<String, String> {
        &self.options
    }
}
```

These methods validate nothing, compute nothing, hide no meaningful representation, and enforce no mutation
policy. They add API surface and navigation without behavior. When `Metadata` is a plain public data model,
prefer direct fields:

```rust
pub struct Metadata {
    pub labels: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
}
```

Use accessors only when they preserve an invariant, compute or normalize a value, restrict mutation, hide a
representation callers must not depend on, or implement a stable trait contract. “We may need encapsulation
later” is not current value.

## A command entity is not automatically cross-platform

```rust
pub struct Tar {
    executable: PathBuf,
}

impl Tar {
    pub fn extract(&self, destination: &Path, input: Stdio) -> io::Result<Child> {
        Command::new(&self.executable)
            .arg("--numeric-owner")
            .arg("-p")
            .arg("-C")
            .arg(destination)
            .stdin(input)
            .spawn()
    }
}
```

Wrapping `Command` correctly classifies implementation behavior, but it does not make the implementation
portable. This code assumes a host `tar` executable, compatible flags, Unix numeric ownership, permission
bits, process pipes, and matching archive semantics. Executable and flag behavior can differ across GNU tar,
BSD tar, macOS, Linux, and Windows.

Keep a platform-neutral archive contract above concrete adapters:

```rust
pub trait Archives {
    type Error;

    fn create(&self, source: &Source) -> Result<Archive, Self::Error>;
    fn extract(
        &self,
        archive: &Archive,
        destination: &Path,
    ) -> Result<(), Self::Error>;
}
```

Composition selects a tested adapter such as a host-command implementation or an in-process archive
implementation. Do not claim portability because platform assumptions were moved behind a struct. Verify the
same behavioral contract independently on every supported host.

## Do not repeat the receiver type in method names

```rust
pub trait Directory {
    type Error;

    fn create_directory(
        &self,
        path: &RelativePath,
    ) -> Result<(), Self::Error>;

    fn create_file(
        &self,
        path: &RelativePath,
    ) -> Result<File, Self::Error>;

    fn create_symlink(
        &self,
        path: &RelativePath,
        target: &RelativePath,
    ) -> Result<(), Self::Error>;
}
```

`Directory` already supplies the namespace. `create_directory` repeats its receiver and should be
`Directory::create`. Apply the same rule to prefixes such as `directory_create` and suffixes such as
`remove_workspace` on `Workspace`.

Do not shorten unrelated concepts mechanically: `Directory::create_file` may remain distinct when it creates
a `File`, while `Directory::create` creates the receiver's primary child concept. The lint finding requires
semantic review when token overlap is meaningful rather than redundant.

## Do not wrap a standard collection without additional meaning

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shell {
    arguments: Vec<String>,
}
```

If this type only stores shell arguments and forwards vector access, it adds a constructor, accessors,
conversions, and navigation without enforcing a meaningful invariant or owning cohesive behavior. Use the
collection directly:

```rust
let shell: Vec<String> = vec!["/bin/sh".into(), "-c".into()];
let arguments = command.resolve(&shell)?;
```

A wrapper becomes justified only when it prevents meaningful value mixing, validates a stable contract,
owns several cohesive operations, or forms a real architectural boundary. Domain-driven naming is not a
reason to wrap every primitive or collection.
