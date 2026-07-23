# Positive design examples

Approved transformations for design-lint cases. Append new examples only after user approval.

## Store key owns filesystem-safe image names
### Error

`safe_name(&ImageRef)` and `encode_store_component(&str)` are detached functions. The first appears to
belong to `ImageRef`, but filesystem representation is storage policy rather than image-reference identity.
The encoding is reused by image roots, sidecar configuration, archives, and build storage.

### Original

```rust
pub fn encode_store_component(name: &str) -> String {
    let encoded = name
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace(':', "%3A");

    match encoded.as_str() {
        "" => "%2E".to_string(),
        "." => "%2E".to_string(),
        ".." => "%2E%2E".to_string(),
        _ => encoded,
    }
}

pub fn safe_name(reference: &ImageRef) -> String {
    encode_store_component(&reference.canonical())
}
```

Callers reconstruct storage layout:

```rust
pub fn rootfs_path(&self, reference: &ImageRef) -> PathBuf {
    PathBuf::from(format!("{}/{}/rootfs", self.dir, safe_name(reference)))
}

let rootfs = PathBuf::from(format!(
    "{images_dir}/{}/rootfs",
    safe_name(&reference)
));
```

### Decision

Extract an invariant-bearing store key. `ImageRef` continues to own reference identity. `Key` owns the
injective, filesystem-safe representation. `Store` owns complete path layout. No classification macro remains.

### After

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Key(String);

impl Key {
    pub fn from_reference(reference: &ImageRef) -> Self {
        Self::from_name(&reference.canonical())
    }

    pub fn from_name(name: &str) -> Self {
        let encoded = name
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace(':', "%3A");

        Self(match encoded.as_str() {
            "" | "." => "%2E".to_owned(),
            ".." => "%2E%2E".to_owned(),
            _ => encoded,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

```rust
impl Store {
    pub fn rootfs_path(&self, reference: &ImageRef) -> PathBuf {
        self.path(reference).join("rootfs")
    }

    pub fn config_path(&self, reference: &ImageRef) -> PathBuf {
        self.path(reference).join("hl-image.json")
    }

    fn path(&self, reference: &ImageRef) -> PathBuf {
        Path::new(&self.dir).join(Key::from_reference(reference).as_str())
    }
}
```

Callers use the store API instead of duplicating its layout:

```rust
let store = Store::new(images_dir);
let rootfs = store.rootfs_path(&reference);
let config = store.config_path(&reference);
```

Raw archive or cache names use `Key::from_name(name)`.

## Put intrinsic identity on the entity

### Error

`image_id(&Image)` is detached from the object whose content identity it computes. It is reused by image
listing, inspection, history, deletion, and tests.

### Original

```rust
pub(crate) fn image_id(image: &Image) -> String {
    let mut labels: Vec<(&String, &String)> = image.labels.iter().collect();
    labels.sort();

    let labels = labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");

    let manifest = format!(
        "rootfs:{}\narch:{}\ncmd:{}\nentrypoint:{}\nenv:{}\nworkdir:{}\nuser:{}\nlabels:\n{}",
        image.rootfs,
        image.arch.arch(),
        image.cmd.join("\u{1}"),
        image.entrypoint.join("\u{1}"),
        image.env.join("\u{1}"),
        image.workdir,
        image.user,
        labels,
    );

    format!(
        "sha256:{}",
        hl_images::build::sha256_hex(manifest.as_bytes())
    )
}
```

```rust
ImageSummary {
    id: image_id(image),
    // ...
}

report.push(DeleteRecord::Deleted(image_id(&target)));
```

### Decision

Content identity is intrinsic `Image` behavior, independent of transport and endpoint policy. The existing
entity is already the correct owner; no wrapper or provisional classification is needed.

### After

```rust
impl Image {
    /// Stable content identity shared by every tag alias.
    pub(crate) fn id(&self) -> String {
        let mut labels: Vec<(&String, &String)> = self.labels.iter().collect();
        labels.sort();

        let labels = labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n");

        let manifest = format!(
            "rootfs:{}\narch:{}\ncmd:{}\nentrypoint:{}\nenv:{}\nworkdir:{}\nuser:{}\nlabels:\n{}",
            self.rootfs,
            self.arch.arch(),
            self.cmd.join("\u{1}"),
            self.entrypoint.join("\u{1}"),
            self.env.join("\u{1}"),
            self.workdir,
            self.user,
            labels,
        );

        format!(
            "sha256:{}",
            hl_images::build::sha256_hex(manifest.as_bytes())
        )
    }
}
```

```rust
ImageSummary {
    id: image.id(),
    // ...
}

report.push(DeleteRecord::Deleted(target.id()));
```

## Specialize an entity through composition

When a domain has a base entity and specialized forms, give the base its own module and let specialized
entities wrap it. Rust uses composition rather than class inheritance.

```text
layer/
  mod.rs
  layer.rs
  hidden.rs
```

```rust
// layer/layer.rs
pub struct Layer {
    id: Id,
}
```

```rust
// layer/hidden.rs
pub struct Hidden {
    layer: Layer,
}

impl Hidden {
    pub fn from_layer(layer: Layer) -> Self {
        Self { layer }
    }

    pub fn layer(&self) -> &Layer {
        &self.layer
    }

    pub fn into_layer(self) -> Layer {
        self.layer
    }
}
```

If `Layer` and `Hidden` must satisfy the same capability, define a narrow trait and implement it for both.
Do not use `Deref` merely to simulate inheritance; expose the base deliberately or forward the required
behavior.

## Put an established score on the scored entity

### Error

Two free functions compute metadata-completeness scores from existing image entities:

```rust
pub fn image_score(image: &DiscoveredImage) -> i32 {
    let mut score = 0;
    if !image.env.is_empty() {
        score += 1000;
    }
    if !image.entrypoint.is_empty() {
        score += 10;
    }
    if !image.workdir.is_empty() {
        score += 5;
    }
    if image.cmd.len() != 1 || image.cmd[0] != "/bin/sh" {
        score += 1;
    }
    score
}
```

```rust
images.sort_by(|left, right| {
    image_score(right)
        .cmp(&image_score(left))
        .then_with(|| left.name.cmp(&right.name))
});
```

### Decision

The calculation reads one existing entity, has one established meaning for that entity, and is independently
tested. Do not invent a ranking trait or provisional classification merely because a future policy might
exist. Extract a policy only when multiple real ranking strategies appear.

### After

```rust
impl DiscoveredImage {
    /// Ranks metadata completeness when duplicate discoveries represent the same image reference.
    pub fn score(&self) -> i32 {
        let mut score = 0;
        if !self.env.is_empty() {
            score += 1000;
        }
        if !self.entrypoint.is_empty() {
            score += 10;
        }
        if !self.workdir.is_empty() {
            score += 5;
        }
        if self.cmd.len() != 1 || self.cmd[0] != "/bin/sh" {
            score += 1;
        }
        score
    }
}
```

```rust
images.sort_by(|left, right| {
    right
        .score()
        .cmp(&left.score())
        .then_with(|| left.name.cmp(&right.name))
});
```

The daemon's separate `Image` model owns its slightly different score through `Image::score()`. No shared
trait is introduced until the domain contains multiple actual ranking policies.

## Replace string layer identities with a value type

### Error

`layer_short(&str) -> String` produces a value that is carried as layer identity through registry metadata,
pull events, push events, progress reporting, and serialization.

```rust
pub fn layer_short(digest: &str) -> String {
    digest
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect()
}
```

```rust
let id = layer_short(&digest);
metas.push(LayerMeta { digest, size, id });
```

### Decision

The result is not arbitrary abbreviated text. It is an identity already propagated across several domain
objects. Give it a type and convert it to a string only at protocol boundaries.

### After

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LayerId(String);

impl LayerId {
    pub fn from_digest(digest: &str) -> Self {
        Self(
            digest
                .trim_start_matches("sha256:")
                .chars()
                .take(12)
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}
```

```rust
pub enum PullEvent {
    Pulling { id: LayerId },
    Downloading {
        id: LayerId,
        current: u64,
        total: u64,
    },
    Extracting {
        id: LayerId,
        current: u64,
        total: u64,
    },
    Complete { id: LayerId },
}
```

```rust
struct LayerMeta {
    digest: String,
    size: u64,
    id: LayerId,
}

let id = LayerId::from_digest(&digest);
metas.push(LayerMeta { digest, size, id });
```

The wire adapter performs the final conversion:

```rust
StreamStatus {
    status: PushStatus::Preparing.into(),
    id: Some(layer_id.as_str().to_owned()),
    progress_detail: None,
}
```

The repeated `Preparing`, `Pushing`, and `Pushed` vocabulary must separately be reviewed as a closed enum;
the layer-identity refactor must not preserve an obvious stringly typed state API merely because it is outside
the original lint case.

## Keep domain models independent from toolkit components

### Before

```rust
fn arch_chip(arch: Arch) -> gtk::Label {
    let label = gtk::Label::new(Some(arch.os_arch_label()));
    label.add_css_class("chip");
    label.add_css_class(match arch {
        Arch::Arm64 => "arm",
        Arch::Amd64 => "amd",
    });
    label.set_valign(gtk::Align::Center);
    label
}
```

Both `dash_settings` and `dash_overview` call `arch_chip(ws.arch)`. Moving this function to
`Arch::chip()` would silence the receiver-design finding while introducing the wrong dependency: the
architecture domain model would construct GTK widgets and own product presentation.

### Decision

Split domain-to-view translation from generic component rendering. Husklet owns the application-specific
architecture view model; `hl-gui` owns the reusable chip component. The free helper then disappears without
polluting `Arch` with GTK behavior.

### After

```rust
// apps/husklet/.../view/architecture.rs
pub struct Architecture {
    label: &'static str,
    accent: Accent,
}

impl From<Arch> for Architecture {
    fn from(arch: Arch) -> Self {
        match arch {
            Arch::Arm64 => Self {
                label: "linux/aarch64",
                accent: Accent::Arm,
            },
            Arch::Amd64 => Self {
                label: "linux/amd64",
                accent: Accent::Amd,
            },
        }
    }
}
```

```rust
// workspaces/hl-gui/.../chip.rs
pub struct Chip {
    label: String,
    accent: Accent,
}

impl Chip {
    pub fn new(label: impl Into<String>) -> Self {
        // Construct toolkit-independent component state.
    }

    pub fn accent(mut self, accent: Accent) -> Self {
        self.accent = accent;
        self
    }
}
```

```rust
let architecture = Architecture::from(ws.arch);
head.append(
    &Chip::new(architecture.label)
        .accent(architecture.accent)
        .widget(),
);
```

Do not add a classification macro. The existing receiver owns domain behavior, but not toolkit rendering.

## Model static product catalogs as typed data

### Before

```rust
fn curated_images(_arch: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "Ubuntu 24.04 LTS",
            "ubuntu:24.04",
            "Latest Ubuntu LTS — the default dev base.",
        ),
        ("Ubuntu 22.04 LTS", "ubuntu:22.04", "Previous Ubuntu LTS."),
        ("Debian 12 (Bookworm)", "debian:bookworm", "Stable Debian."),
        ("Alpine", "alpine:latest", "Tiny musl-based image."),
        ("Fedora", "fedora:latest", "Fedora — recent packages."),
        (
            "AlmaLinux 9",
            "almalinux:9",
            "RHEL-compatible enterprise base.",
        ),
    ]
}
```

The argument is ignored, the tuple hides three domain concepts, and every call allocates a new `Vec` for
unchanging data. The catalog is Husklet product policy, not generic container-image behavior.

### Decision

Represent the catalog as typed static application data. Remove the false architecture-sensitive API. Do not
add `Catalog::for_arch` unless catalogs actually differ by architecture.

### After

```rust
// apps/husklet/.../image/template.rs
pub struct Template {
    pub name: &'static str,
    pub reference: &'static str,
    pub description: &'static str,
}
```

```rust
// apps/husklet/.../image/mod.rs
pub const CURATED: &[Template] = &[
    Template {
        name: "Ubuntu 24.04 LTS",
        reference: "ubuntu:24.04",
        description: "Latest Ubuntu LTS — the default dev base.",
    },
    Template {
        name: "Ubuntu 22.04 LTS",
        reference: "ubuntu:22.04",
        description: "Previous Ubuntu LTS.",
    },
    Template {
        name: "Debian 12 (Bookworm)",
        reference: "debian:bookworm",
        description: "Stable Debian.",
    },
];
```

```rust
for template in image::CURATED {
    let name = gtk::Label::new(Some(template.name));
    let details = gtk::Label::new(Some(&format!(
        "{}  ·  {}",
        template.reference,
        template.description,
    )));

    let reference = template.reference;
    click.connect_released(move |_, _, _, _| {
        form.image.set_text(reference);
        window.close();
    });
}
```

Remove `curated_images`; no classification macro. Introduce `image::Catalog::for_arch(Arch)` only after
architecture-specific catalog behavior exists.

## Separate a generic dialog from product confirmation policy

### Before

```rust
fn confirm_dialog(message: &str) -> bool {
    let script = format!(
        "display dialog \"{message}\" buttons {{\"Cancel\", \"Delete\"}} \
         default button \"Cancel\" with icon caution"
    );

    std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("Delete")
        })
        .unwrap_or(false)
}
```

This generic-looking helper hides a platform process, interpolates unescaped text into AppleScript,
hardcodes product actions, blocks the caller, parses stringly typed output, and collapses cancellation and
failure into `false`.

### Decision

`hl-gui` owns a toolkit-independent `Dialog` component and a narrow presentation port. A reusable
confirmation variant may live under `component/dialog/confirm.rs` once it has cohesive semantics. The
workspace feature owns the concrete removal prompt and Husklet owns its effect. The macOS adapter translates
the typed dialog directly to the native API without shell or AppleScript.

### After

```rust
pub struct Dialog {
    title: String,
    message: String,
    actions: Vec<Action>,
    default: ActionId,
    severity: Severity,
}

pub struct Action {
    id: ActionId,
    label: String,
    role: Role,
}

pub enum Role {
    Cancel,
    Confirm,
    Destructive,
}

pub enum Severity {
    Information,
    Warning,
    Critical,
}

pub enum Response {
    Selected(ActionId),
    Dismissed,
}
```

```rust
pub trait Dialogs {
    type Error;

    async fn present(
        &self,
        parent: &Window,
        dialog: Dialog,
    ) -> Result<Response, Self::Error>;
}
```

The workspace feature composes the generic component:

```rust
let dialog = Dialog::new("Remove workspace")
    .message(format!(
        "Remove workspace {name}? Its files on disk will be kept."
    ))
    .action(Action::cancel())
    .action(Action::destructive("remove", "Delete"))
    .default("cancel")
    .severity(Severity::Warning);

match dialogs.present(&window, dialog).await? {
    Response::Selected(action) if action == "remove" => {
        store.remove(&name)?;
        workspaces.refresh();
    }
    Response::Selected(_) | Response::Dismissed => {}
}
```

Remove `confirm_dialog`; no classification macro. Do not replace it with `Dialog::confirm(message) -> bool`:
that would retain hardcoded confirmation policy and erase meaningful outcomes.

## Keep feature components independent from their containing form

### Before

```rust
fn open_image_picker(parent: &gtk::Window, form: &Rc<Form>) {
    let arch = if form.cpu_amd.get() {
        "amd64"
    } else {
        "arm64"
    };

    let window = gtk::Window::builder()
        .title("Choose an image")
        .modal(true)
        .transient_for(parent)
        .build();

    for (name, reference, description) in curated_images(arch) {
        // Construct GTK rows.
        let form = form.clone();
        let window = window.clone();
        click.connect_released(move |_, _, _, _| {
            form.image.set_text(reference);
            window.close();
        });
    }

    window.present();
    macshim::force_dark();
}
```

The function mixes product catalog policy, image-picker behavior, generic modal/list/button construction,
GTK presentation, and macOS behavior. It receives the entire `Form` only to read one value and mutate one
input, coupling the feature to a particular page implementation.

### Decision

Keep the image picker beside the Husklet image feature. Give it typed inputs and typed events. Compose it
from generic `hl-gui` components, and let the containing page decide how selection updates its form.

```text
apps/husklet/src/
  image/
    template.rs
    component/
      picker.rs

workspaces/hl-gui/src/
  component/
    button.rs
    list.rs
    modal.rs
```

### After

```rust
pub struct Picker {
    architecture: Arch,
    templates: &'static [Template],
}

pub enum Event {
    Selected(ImageRef),
    Cancelled,
}

impl Picker {
    pub fn new(
        architecture: Arch,
        templates: &'static [Template],
    ) -> Self {
        Self {
            architecture,
            templates,
        }
    }

    pub fn view(&self) -> Modal<Event> {
        Modal::new("Choose an image")
            .subtitle(format!(
                "for {} — or cancel and type a custom image reference",
                self.architecture
            ))
            .content(
                List::new(self.templates)
                    .row(template_row)
                    .on_select(|template| {
                        Event::Selected(template.reference.parse()?)
                    }),
            )
            .action(Button::cancel(Event::Cancelled))
    }
}
```

The page connects intent to its own state:

```rust
let picker = Picker::new(form.architecture(), image::CURATED);

dialogs.present(parent, picker.view(), move |event| match event {
    image::picker::Event::Selected(reference) => form.set_image(reference),
    image::picker::Event::Cancelled => {}
});
```

Remove `open_image_picker`; no classification macro. `Picker` knows image-selection behavior but not `Form`;
generic components know no image concepts; native presentation remains behind the GUI adapter.

## Model a settings pane as typed state and events

### Before

```rust
fn pane_general(form: &Rc<Form>) -> gtk::Box {
    let pane = pane("General");

    pane.append(&field(
        "NAME",
        &form.name,
        Some("A friendly name for this workspace."),
    ));

    // Construct architecture widgets and mutate form.cpu_amd.
    // Construct image widgets and invoke open_image_picker.
    // Construct storage widgets and invoke a macOS folder picker.

    pane
}
```

The parent then validates by reading mutable GTK widgets. `Form` is acting as application state, and the
pane mixes rendering, state transitions, validation, feature dialogs, generic controls, and platform I/O.

### Decision

Keep workspace draft state and events in the Husklet workspace-creation feature. Compose its `General`
component from generic `hl-gui` settings controls. The page updates the draft and emits effect commands;
Husklet executes those commands through ports. Validate the typed draft, never the widget tree.

```text
apps/husklet/src/
  workspace/
    create/
      page.rs
      draft.rs
      general.rs
      event.rs

workspaces/hl-gui/src/
  component/
    field.rs
    input.rs
    segmented.rs
    file.rs
    settings.rs
```

### After

```rust
pub struct Draft {
    pub name: WorkspaceName,
    pub architecture: Arch,
    pub image: Option<ImageRef>,
    pub shell: Option<Shell>,
    pub storage: Option<PathBuf>,
}
```

```rust
pub struct General<'a> {
    draft: &'a Draft,
    errors: &'a Errors,
}

pub enum Event {
    NameChanged(String),
    ArchitectureChanged(Arch),
    ImageChanged(String),
    ChooseImage,
    ShellChanged(String),
    StorageChanged(PathBuf),
    BrowseStorage,
}

impl<'a> General<'a> {
    pub fn new(draft: &'a Draft, errors: &'a Errors) -> Self {
        Self { draft, errors }
    }

    pub fn view(&self) -> Settings<Event> {
        Settings::new("General")
            .field(
                Field::new("Name")
                    .description("A friendly name for this workspace.")
                    .error(self.errors.name())
                    .content(
                        Input::new(self.draft.name.as_str())
                            .on_change(Event::NameChanged),
                    ),
            )
            .field(
                Field::new("Architecture").content(
                    Segmented::new(self.draft.architecture)
                        .option(Arch::Arm64, "arm64")
                        .option(Arch::Amd64, "x86-64")
                        .on_change(Event::ArchitectureChanged),
                ),
            )
            .field(
                Field::new("Image")
                    .description("Choose a template or enter an image reference.")
                    .error(self.errors.image())
                    .content(
                        Input::new(self.draft.image_text())
                            .on_change(Event::ImageChanged)
                            .action("Choose…", Event::ChooseImage),
                    ),
            )
            .field(
                Field::new("Storage location").content(
                    FileInput::new(self.draft.storage.as_deref())
                        .on_change(Event::StorageChanged)
                        .on_browse(Event::BrowseStorage),
                ),
            )
    }
}
```

The page owns transitions and describes effects:

```rust
impl Create {
    fn update(&mut self, event: general::Event) -> Command {
        match event {
            general::Event::NameChanged(name) => {
                self.draft.set_name(name);
                Command::None
            }
            general::Event::ArchitectureChanged(arch) => {
                self.draft.set_architecture(arch);
                Command::None
            }
            general::Event::ImageChanged(reference) => {
                self.draft.set_image_text(reference);
                Command::None
            }
            general::Event::ChooseImage => {
                Command::ChooseImage(self.draft.architecture)
            }
            general::Event::BrowseStorage => Command::ChooseFolder,
            _ => Command::None,
        }
    }
}
```

Husklet executes those effects through ports and validates the model:

```rust
match page.update(event) {
    Command::ChooseImage(arch) => dialogs.present(
        parent,
        image::Picker::new(arch, image::CURATED).view(),
    ),
    Command::ChooseFolder => files.choose_folder(parent),
    Command::None => {}
}

match WorkspaceConfig::try_from(&page.draft) {
    Ok(config) => workspaces.create(config).await?,
    Err(errors) => page.show(errors),
}
```

Remove `pane_general`; no classification macro. `General` is a feature component; `Settings`, `Field`,
`Input`, `Segmented`, and `FileInput` are generic GUI components.

## Wrap toolkit entities and give their collection behavior

### Before

```rust
thread_local! {
    static TERMS: RefCell<Vec<glib::WeakRef<vte4::Terminal>>> =
        const { RefCell::new(Vec::new()) };
}

fn register_terminal(terminal: &vte4::Terminal) {
    TERMS.with(|terminals| {
        terminals.borrow_mut().push(terminal.downgrade());
    });
}

fn apply_config_to_all() {
    let config = current_config();
    TERMS.with(|terminals| {
        terminals.borrow_mut().retain(|weak| {
            let Some(terminal) = weak.upgrade() else {
                return false;
            };
            style_terminal(&terminal, &config);
            true
        });
    });
}
```

`TERMS` is an unnamed collection entity. Its registration, iteration, pruning, and bulk configuration are
scattered through globals and free functions. The raw `vte4::Terminal` also leaks toolkit ownership into
callers.

### Decision

Wrap one toolkit terminal as `Terminal`. Model the list as `Terminals`, a collection entity with collection
behavior. Keep both in the VTE adapter while they expose VTE types; do not pretend they are
toolkit-independent domain models.

### After

```rust
pub struct Terminal {
    widget: vte4::Terminal,
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            widget: vte4::Terminal::new(),
        }
    }

    pub fn widget(&self) -> &vte4::Terminal {
        &self.widget
    }

    pub fn apply(&self, config: &TermConfig) {
        style_terminal(&self.widget, config);
        if config.scrollback.is_some() {
            self.widget
                .set_scrollback_lines(config.scrollback_lines());
        }
    }
}
```

```rust
pub struct Terminals {
    items: Vec<Weak<Terminal>>,
}

impl Terminals {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn insert(&mut self, terminal: &Rc<Terminal>) {
        self.items.push(Rc::downgrade(terminal));
    }

    pub fn apply(&mut self, config: &TermConfig) {
        self.items.retain(|weak| {
            let Some(terminal) = weak.upgrade() else {
                return false;
            };
            terminal.apply(config);
            true
        });
    }
}
```

The application owns the collection:

```rust
pub struct Application {
    terminals: RefCell<Terminals>,
    config: RefCell<TermConfig>,
}

let terminal = Rc::new(Terminal::new());
application.terminals.borrow_mut().insert(&terminal);
application
    .terminals
    .borrow_mut()
    .apply(&application.config.borrow());
```

Remove `TERMS`, `register_terminal`, and `apply_config_to_all`; no classification macro. `Terminal` owns
one instance's toolkit behavior, while `Terminals` acts as the list and owns collection-wide behavior.

## Parse and display a validated value through standard traits

### Before

```rust
fn is_hex6(value: &str) -> bool {
    let body = value.strip_prefix('#').unwrap_or(value);
    (body.len() == 6 || body.len() == 3)
        && body.chars().all(|character| character.is_ascii_hexdigit())
}
```

```rust
let foreground = ui.fg.text().trim().to_string();
if is_hex6(&foreground) {
    config.foreground = foreground;
}
```

The value is a color, not an arbitrary string. Validation is disconnected from construction, configuration
can still contain invalid strings, the name says six digits while accepting three, and invalid input is
silently discarded.

### Decision

Create a color only through validated construction. Use `FromStr` for textual parsing and `Display` for the
canonical representation. Store the typed value in configuration and translate it to toolkit types at the
adapter boundary.

### After

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color {
    red: u8,
    green: u8,
    blue: u8,
}
```

```rust
impl FromStr for Color {
    type Err = ParseColorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value.strip_prefix('#').unwrap_or(value);

        match hex.len() {
            3 => {
                let mut digits = hex.chars();
                let red = channel(digits.next().unwrap())?;
                let green = channel(digits.next().unwrap())?;
                let blue = channel(digits.next().unwrap())?;

                Ok(Self {
                    red: red * 17,
                    green: green * 17,
                    blue: blue * 17,
                })
            }
            6 => Ok(Self {
                red: pair(&hex[0..2])?,
                green: pair(&hex[2..4])?,
                blue: pair(&hex[4..6])?,
            }),
            length => Err(ParseColorError::Length(length)),
        }
    }
}
```

```rust
impl Display for Color {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "#{:02x}{:02x}{:02x}",
            self.red,
            self.green,
            self.blue,
        )
    }
}
```

```rust
pub struct TermConfig {
    pub foreground: Color,
    pub background: Color,
}

let foreground = ui
    .foreground()
    .parse::<Color>()
    .map_err(SettingsError::foreground)?;

let background = ui
    .background()
    .parse::<Color>()
    .map_err(SettingsError::background)?;

config.set_foreground(foreground);
config.set_background(background);
```

```rust
impl From<Color> for gdk::RGBA {
    fn from(color: Color) -> Self {
        // Adapter-specific channel conversion.
    }
}
```

Remove `is_hex6`; no classification macro. Keep `Color` in the generic GUI model while that domain owns the
complete concept; extract a precise package only after other domains share the same invariants.

## Inline a helper that only renames an existing method

### Before

```rust
fn log_bytes(output: LogOutput) -> bytes::Bytes {
    output.into_bytes()
}
```

Its only caller adds another name for the same operation:

```rust
while let Some(item) = stream.next().await {
    out.extend_from_slice(&log_bytes(item?));
}
```

### Decision

`LogOutput::into_bytes()` already classifies the conversion on the correct receiver. A one-use forwarding
helper adds navigation but no concept, policy, reuse, or isolation.

### After

```rust
while let Some(item) = stream.next().await {
    let output = item?;
    out.extend_from_slice(&output.into_bytes());
}
```

Remove `log_bytes`; no classification macro and no new type. Do not create another conversion trait or
wrapper when the dependency already exposes the correct receiver method.

## Keep a transferable domain value in the owning domain library

### Before

```rust
pub fn short(id: &str) -> String {
    id.trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect()
}
```

Container, image, and network models pass raw identifier strings through the same SHA-256 display rule.

### Decision

The identifier has a precise, transferable meaning inside the container domain, but that does not justify a
general `hl-hash` package. Put `Sha256Id` in the owning container crate's domain `lib`, for example
`hl-daemon/src/lib/sha_id.rs` when the daemon owns the value, or the equivalent location in the protocol/client
owner. Do not add phantom entity kinds or a generic ID hierarchy without evidence that cross-entity mixing is
a real problem.

### After

```text
src/containers/hl-daemon/src/
  lib/
    sha_id.rs
```

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Sha256Id(String);

impl Sha256Id {
    pub fn parse(value: impl Into<String>) -> Result<Self, ParseSha256IdError> {
        let value = value.into();
        let body = value.strip_prefix("sha256:").unwrap_or(&value);

        if body.is_empty()
            || !body
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(ParseSha256IdError::Invalid(value));
        }

        Ok(Self(value))
    }

    pub fn short(&self) -> &str {
        let body = self.0.strip_prefix("sha256:").unwrap_or(&self.0);
        &body[..body.len().min(12)]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

```rust
impl Display for Sha256Id {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
```

```rust
pub struct Container {
    pub id: Sha256Id,
}

container.id.short();
```

Remove the free `short` helper; no classification macro. The important placement rule is domain `lib`
before a repository-wide package: reusable within container implementations does not mean generally reusable
outside the container domain.

## Compose entities from shared metadata and use the ordered collection directly

### Before

```rust
pub(super) fn sorted_pairs(
    map: HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut pairs: Vec<_> = map.into_iter().collect();
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    pairs
}
```

```rust
pub struct Network {
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
}

pub struct Volume {
    pub labels: Vec<(String, String)>,
    pub options: Vec<(String, String)>,
}
```

The helper manually recreates `BTreeMap` ordering, while `Network` and `Volume` duplicate the cohesive
labels/options basis.

### Decision

Model the basis as `Metadata` even though it currently has only two fields: cohesion and shared meaning, not
a mechanical field threshold, justify composition. Use `BTreeMap` because its standard contract already
provides unique keys and stable iteration. Keep fields direct when accessors would add no behavior.

### After

```rust
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    pub labels: BTreeMap<String, String>,
    pub options: BTreeMap<String, String>,
}

impl Metadata {
    pub fn new(
        labels: HashMap<String, String>,
        options: HashMap<String, String>,
    ) -> Self {
        Self {
            labels: labels.into_iter().collect(),
            options: options.into_iter().collect(),
        }
    }
}
```

```rust
pub struct Network {
    pub metadata: Metadata,
    // Network-specific fields.
}

pub struct Volume {
    pub metadata: Metadata,
    // Volume-specific fields.
}
```

```rust
Network {
    metadata: Metadata::new(
        network.labels.unwrap_or_default(),
        network.options.unwrap_or_default(),
    ),
    // ...
}
```

```rust
for (key, value) in &network.metadata.labels {
    row.add(key, value);
}
```

Remove `sorted_pairs`; no classification macro. `Metadata` owns the shared basis and ordering invariant;
`Network` and `Volume` extend it through composition. Do not add accessors that only return these fields.

## Put an external command and its compatibility flags on one entity

### Before

```rust
fn get_tar_argv(parent: &Path, base: &str) -> Vec<OsString> {
    vec![
        "--format=posix".into(),
        "-c".into(),
        "-f".into(),
        "-".into(),
        "-C".into(),
        parent.into(),
        base.into(),
    ]
}

fn put_tar_argv(host: &Path) -> Vec<OsString> {
    vec![
        "-x".into(),
        "-f".into(),
        "-".into(),
        "--numeric-owner".into(),
        "-p".into(),
        "-C".into(),
        host.into(),
    ]
}
```

Handlers separately choose `Command::new("tar")`. Process ownership is split, HTTP `get`/`put` vocabulary
leaks into archive mechanics, and argument vectors expose implementation details.

### Decision

One concrete `Tar` adapter owns the executable, invocation, compatibility flags, and process lifecycle. Use
archive verbs such as `create` and `extract`. Verify required behavior instead of asserting that particular
argument strings occur.

### After

```rust
pub struct Tar {
    executable: PathBuf,
}

impl Tar {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn create(
        &self,
        parent: &Path,
        member: &OsStr,
    ) -> io::Result<Output> {
        Command::new(&self.executable)
            .arg("--format=posix")
            .arg("-c")
            .arg("-f")
            .arg("-")
            .arg("-C")
            .arg(parent)
            .arg(member)
            .output()
    }

    pub fn extract(
        &self,
        destination: &Path,
        input: Stdio,
    ) -> io::Result<Child> {
        Command::new(&self.executable)
            .arg("-x")
            .arg("-f")
            .arg("-")
            .arg("--numeric-owner")
            .arg("-p")
            .arg("-C")
            .arg(destination)
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    }
}
```

```rust
let output = tar.create(&parent, base.as_ref())?;
let child = tar.extract(host, Stdio::piped())?;
```

Behavioral compatibility tests assert that archive round trips preserve nanosecond timestamps, permission
modes, and other promised semantics. Remove `get_tar_argv` and `put_tar_argv`; no classification macros.

This positive example covers ownership of one concrete mechanism. It does not establish cross-platform
support; see the corresponding negative example.

## Parse security-sensitive archives as typed data and fail closed

### Before

```rust
fn list_tar_entries(body: &[u8]) -> Vec<String> {
    let child = Command::new("tar")
        .arg("-t")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let Ok(mut child) = child else {
        return Vec::new();
    };

    match child.wait_with_output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect(),
        Err(_) => Vec::new(),
    }
}
```

```rust
scrub_traversal_symlinks(destination, &list_tar_entries(body));
extract_archive(destination, body)?;
```

Process or parsing failure becomes an empty list and extraction continues, bypassing the preparation that
depends on the member list. Lossy newline-delimited output also cannot represent every archive path and
discards entry kinds and link targets.

### Decision

Model the immutable archive bytes as `Archive`. Parse entries in-process into typed paths, kinds, and link
targets. Return errors and prevent extraction whenever inspection or validation fails.

### After

```rust
pub struct Archive<'a> {
    bytes: &'a [u8],
}

pub struct Entry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub link: Option<PathBuf>,
}

pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Hardlink,
}
```

```rust
impl<'a> Archive<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn entries(&self) -> Result<Vec<Entry>, ArchiveError> {
        let reader = Cursor::new(self.bytes);
        let mut archive = tar::Archive::new(reader);
        let mut entries = Vec::new();

        for entry in archive.entries()? {
            let entry = entry?;
            entries.push(Entry {
                path: entry.path()?.into_owned(),
                kind: EntryKind::from(entry.header().entry_type()),
                link: entry.link_name()?.map(Cow::into_owned),
            });
        }

        Ok(entries)
    }

    pub fn validate(&self, destination: &Path) -> Result<(), ArchiveError> {
        for entry in self.entries()? {
            destination.validate_member(&entry.path)?;
            if let Some(link) = &entry.link {
                destination.validate_link(&entry.path, link)?;
            }
        }
        Ok(())
    }
}
```

```rust
let archive = Archive::new(body);
archive.validate(destination)?;
archives.extract(&archive, destination)?;
```

Remove `list_tar_entries`; no classification macro. Tests cover invalid archives, newlines, non-UTF-8 paths,
absolute and parent traversal, and escaping symbolic and hard links. An unreadable archive must never be
treated as an archive with no entries when inspection controls extraction safety.

## Put a protocol representation on its wire type

### Before

```rust
pub(crate) fn go_filemode(metadata: &fs::Metadata) -> u32 {
    let unix = metadata.permissions().mode();
    let mut mode = unix & 0o777;

    if metadata.file_type().is_dir() {
        mode |= 1 << 31;
    }
    if metadata.file_type().is_symlink() {
        mode |= 1 << 27;
    }

    // More protocol bit translation.
    mode
}
```

The function translates host metadata into the exact high-bit layout required by the Docker-compatible
path-stat protocol. It is neither generic filesystem behavior nor an arbitrary integer helper.

### Decision

The wire API owns a typed `FileMode`. Use named constants for protocol bits and `From` for the complete,
policy-free conversion. Translate the newtype back to its serialized integer through `From`, not a trivial
getter.

### After

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct FileMode(u32);

impl FileMode {
    const DIRECTORY: u32 = 1 << 31;
    const SYMLINK: u32 = 1 << 27;
    const DEVICE: u32 = 1 << 26;
    const NAMED_PIPE: u32 = 1 << 25;
    const SOCKET: u32 = 1 << 24;
    const SET_USER_ID: u32 = 1 << 23;
    const SET_GROUP_ID: u32 = 1 << 22;
    const CHARACTER_DEVICE: u32 = 1 << 21;
    const STICKY: u32 = 1 << 20;
}
```

```rust
#[cfg(unix)]
impl From<&fs::Metadata> for FileMode {
    fn from(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let unix = metadata.permissions().mode();
        let kind = metadata.file_type();
        let mut mode = unix & 0o777;

        if kind.is_dir() {
            mode |= Self::DIRECTORY;
        }
        if kind.is_symlink() {
            mode |= Self::SYMLINK;
        }
        if kind.is_fifo() {
            mode |= Self::NAMED_PIPE;
        }
        if kind.is_socket() {
            mode |= Self::SOCKET;
        }
        if kind.is_block_device() {
            mode |= Self::DEVICE;
        }
        if kind.is_char_device() {
            mode |= Self::DEVICE | Self::CHARACTER_DEVICE;
        }
        if unix & 0o4000 != 0 {
            mode |= Self::SET_USER_ID;
        }
        if unix & 0o2000 != 0 {
            mode |= Self::SET_GROUP_ID;
        }
        if unix & 0o1000 != 0 {
            mode |= Self::STICKY;
        }

        Self(mode)
    }
}

impl From<FileMode> for u32 {
    fn from(mode: FileMode) -> Self {
        mode.0
    }
}
```

```rust
#[derive(Serialize)]
pub struct PathStat {
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub mtime: String,
    #[serde(rename = "linkTarget")]
    pub link_target: String,
}

let stat = PathStat {
    name,
    size: metadata.len(),
    mode: FileMode::from(&metadata).into(),
    mtime,
    link_target,
};
```

Remove `go_filemode`; no classification macro. Keep `FileMode` beside the Docker-compatible archive wire
model. Platform metadata translation remains explicitly gated and requires a tested implementation for each
supported host.

## Separate a typed wire value from fallible header encoding

### Before

```rust
pub(crate) fn path_stat_b64(host: &Path) -> Option<String> {
    let metadata = fs::symlink_metadata(host).ok()?;
    let name = host
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let link = if metadata.file_type().is_symlink() {
        fs::read_link(host)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let stat = json!({
        "name": name,
        "size": metadata.len(),
        "mode": go_filemode(&metadata),
        "mtime": fmt_rfc3339(metadata.mtime()),
        "linkTarget": link,
    });
    Some(base64_std(stat.to_string().as_bytes()))
}
```

`Option` conflates every failure, anonymous JSON bypasses checked protocol fields, filesystem errors are
silenced, lossy path conversion becomes accidental policy, and an HTTP header is returned as an arbitrary
string.

### Decision

Model the wire payload as `PathStat`, make filesystem construction return typed errors, and convert it to the
real HTTP `HeaderValue` through fallible `TryFrom`. Do not use `Display`: base64 JSON encoding is a fallible
protocol operation, not canonical human-readable text.

### After

```rust
#[derive(Clone, Debug, Serialize)]
pub struct PathStat {
    pub name: String,
    pub size: u64,
    pub mode: u32,
    pub mtime: String,
    #[serde(rename = "linkTarget")]
    pub link_target: String,
}
```

```rust
impl PathStat {
    pub fn read(path: &Path) -> Result<Self, PathStatError> {
        let metadata = fs::symlink_metadata(path)
            .map_err(PathStatError::metadata)?;
        let name = path
            .file_name()
            .ok_or(PathStatError::MissingName)?
            .to_str()
            .ok_or(PathStatError::InvalidName)?
            .to_owned();

        let link_target = if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .map_err(PathStatError::link)?
                .to_str()
                .ok_or(PathStatError::InvalidLink)?
                .to_owned()
        } else {
            String::new()
        };

        Ok(Self {
            name,
            size: metadata.len(),
            mode: FileMode::from(&metadata).into(),
            mtime: Timestamp::from(&metadata)?.to_string(),
            link_target,
        })
    }
}
```

```rust
impl TryFrom<&PathStat> for HeaderValue {
    type Error = PathStatHeaderError;

    fn try_from(stat: &PathStat) -> Result<Self, Self::Error> {
        let json = serde_json::to_vec(stat)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(json);
        Ok(HeaderValue::from_str(&encoded)?)
    }
}
```

```rust
let stat = match PathStat::read(&host) {
    Ok(stat) => stat,
    Err(PathStatError::NotFound(_)) => {
        return ErrorResponse::not_found(path);
    }
    Err(error) => return ErrorResponse::internal(error),
};

response.headers_mut().insert(
    X_DOCKER_CONTAINER_PATH_STAT,
    HeaderValue::try_from(&stat)?,
);
```

Remove `path_stat_b64`; no classification macro. Typed failures let the endpoint distinguish absence from
internal failure, while the wire owner controls exact serialization.

## Preserve exclusive syntax forms as an enum without wrapping every value

### Before

```rust
fn exec_or_shell(arguments: &str, shell: &[String]) -> Vec<String> {
    let arguments = arguments.trim();

    if arguments.starts_with('[') {
        if let Ok(Value::Array(values)) = serde_json::from_str(arguments) {
            return values
                .into_iter()
                .filter_map(|value| value.as_str().map(String::from))
                .collect();
        }
    }

    let mut output = shell.to_vec();
    output.push(arguments.to_owned());
    output
}
```

Dockerfile exec and shell forms collapse immediately into one vector, and non-string array members are
silently discarded.

### Decision

An enum is justified because the two syntax forms are exclusive and must survive parsing. Keep the current
shell as `Vec<String>` because a wrapper would add no invariant or behavior. Parse with `FromStr`, then
resolve only where execution arguments are required.

### After

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Exec(Vec<String>),
    Shell(String),
}
```

```rust
impl FromStr for Command {
    type Err = ParseCommandError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let source = source.trim();

        let Ok(value) = serde_json::from_str::<serde_json::Value>(source) else {
            return Ok(Self::Shell(source.to_owned()));
        };

        let serde_json::Value::Array(values) = value else {
            return Ok(Self::Shell(source.to_owned()));
        };

        let arguments = values
            .into_iter()
            .map(|value| {
                value
                    .as_str()
                    .map(String::from)
                    .ok_or(ParseCommandError::NonString)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::Exec(arguments))
    }
}
```

```rust
impl Command {
    pub fn resolve(&self, shell: &[String]) -> Result<Vec<String>, ResolveError> {
        match self {
            Self::Exec(arguments) => Ok(arguments.clone()),
            Self::Shell(command) => {
                if shell.is_empty() {
                    return Err(ResolveError::EmptyShell);
                }

                let mut arguments = shell.to_vec();
                arguments.push(command.clone());
                Ok(arguments)
            }
        }
    }
}
```

```rust
let command = source.parse::<Command>()?;
let arguments = command.resolve(&shell)?;
```

Remove `exec_or_shell`; no classification macro. Keep `Command` in the Dockerfile build model because its
variants encode Dockerfile syntax, while the plain vector remains sufficient for the configured shell.

## Extract a mature helper cluster into one ordered rule-set entity

### Before

```rust
fn parse_dockerignore(context: &Path) -> Vec<(bool, String)> {
    let Ok(content) = fs::read_to_string(context.join(".dockerignore")) else {
        return Vec::new();
    };

    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| match line.strip_prefix('!') {
            Some(pattern) => (
                true,
                pattern.trim().trim_matches('/').to_owned(),
            ),
            None => (false, line.trim_matches('/').to_owned()),
        })
        .collect()
}
```

Related free functions `di_wildcard_seg`, `di_seg_match`, `di_pattern_matches`, `di_excluded`, and
`apply_dockerignore` already form one cohesive concept.

### Decision

Model the ordered rule set as `Dockerignore`, individual rules as `Pattern`, inclusion policy as an enum, and
path syntax as segments. Use `FromStr` for text parsing. Keep file I/O separate, distinguish absence from I/O
failure, and apply one filtered context view consistently to hashing, `COPY`, and `ADD` instead of deleting
files as an implicit representation.

### After

```rust
pub struct Dockerignore {
    patterns: Vec<Pattern>,
}

pub struct Pattern {
    action: Action,
    segments: Vec<Segment>,
}

pub enum Action {
    Exclude,
    Include,
}

pub enum Segment {
    Recursive,
    Pattern(String),
}
```

```rust
impl FromStr for Dockerignore {
    type Err = ParseDockerignoreError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let patterns = source
            .lines()
            .enumerate()
            .filter_map(parse_line)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self { patterns })
    }
}
```

```rust
impl Dockerignore {
    pub fn excludes(&self, path: &RelativePath) -> bool {
        let mut excluded = false;

        for pattern in &self.patterns {
            if pattern.matches(path) {
                excluded = pattern.action == Action::Exclude;
            }
        }

        excluded
    }
}

impl Pattern {
    pub fn matches(&self, path: &RelativePath) -> bool {
        // Complete, bounded, segment-aware matching.
    }
}
```

```rust
let dockerignore = match fs::read_to_string(context.join(".dockerignore")) {
    Ok(source) => source.parse::<Dockerignore>()?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
        Dockerignore::default()
    }
    Err(error) => return Err(error.into()),
};

for entry in context.entries()? {
    if !dockerignore.excludes(entry.relative()) {
        copy.add(entry)?;
    }
}
```

Remove the entire `di_*` helper cluster and `apply_dockerignore`; no classification macros. Do not move the
existing recursive matcher unchanged: compatibility tests must cover escaping, negation, `**`, directory
semantics, separators, non-UTF-8 entries, and adversarial patterns without exponential behavior.

## Keep submitted build arguments distinct from resolved stage variables

### Before

```rust
pub(crate) fn parse_build_args(
    raw: Option<&str>,
) -> HashMap<String, String> {
    raw.filter(|value| !value.is_empty())
        .and_then(|value| {
            serde_json::from_str::<HashMap<String, Option<String>>>(value).ok()
        })
        .map(|arguments| {
            arguments
                .into_iter()
                .filter_map(|(name, value)| {
                    value.map(|value| (name, value))
                })
                .collect()
        })
        .unwrap_or_default()
}
```

Invalid JSON silently becomes no arguments, explicit `null` values disappear before policy can interpret
them, and submitted API arguments collapse into the same map type as resolved stage variables.

### Decision

Model the submitted wire value as `BuildArgs`. This wrapper is justified because it separates two values with
different precedence and lifecycles, preserves explicit unset state, owns parsing and resolution policy, and
provides deterministic ordering for cache-sensitive use. Parse with `FromStr` and reject malformed input at
the request boundary.

### After

```rust
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildArgs(
    BTreeMap<String, Option<String>>,
);
```

```rust
impl FromStr for BuildArgs {
    type Err = ParseBuildArgsError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let values = serde_json::from_str::<
            BTreeMap<String, Option<String>>,
        >(source)?;

        Ok(Self(values))
    }
}
```

```rust
let build_args = query
    .buildargs
    .as_deref()
    .filter(|value| !value.is_empty())
    .map(str::parse::<BuildArgs>)
    .transpose()
    .map_err(BuildError::arguments)?
    .unwrap_or_default();
```

```rust
impl BuildArgs {
    pub fn resolve(
        &self,
        name: &str,
        environment: &EnvVars,
    ) -> Option<String> {
        match self.0.get(name) {
            Some(Some(value)) => Some(value.clone()),
            Some(None) => environment.get(name).cloned(),
            None => None,
        }
    }
}
```

Remove `parse_build_args`; no classification macro. Do not eagerly convert `BuildArgs` into the stage's
plain map: submitted arguments, global declarations, stage-scoped values, and environment variables have
different precedence and lifecycles.

## Framework extractors identify transport adapters

### Original

```rust
#[hl_design::adapter]
pub(super) async fn info(
    State(state): State<DockerState>,
) -> ApiResult<Json<SystemInfo>> {
    let containers = state.containers.list().await
        .map_err(ApiError::container)?;
    let images = state.image_summaries().await?;
    Ok(Json(SystemInfo::from_runtime(containers, images)))
}
```

### Decision

Keep the handler free. Axum owns the extractor signature, so it is a transport adapter rather than behavior
to move mechanically onto `DockerState`. Moving it to `DockerState::info()` would mix HTTP response policy
with application state. The handler may coordinate calls needed for one response, but domain calculations
and reusable operations still belong to their entities or services. `#[hl_design::adapter]` records the
reviewed framework boundary; it is not a provisional classification.
