//! What the extension shelf promises: a workspace's own extensions are on its
//! sidebar, our settings page drives the lifecycle policy, an image is only
//! recorded after somebody agreed to what it asks for, and a click on a
//! rendered widget reaches the extension that drew it.

use std::cell::{Cell, RefCell};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use hl::extension::{Acquisition, Candidate, Roster};
use hl_extension::{Capability, ExtensionName, Grant, Manifest, Record, Stage, Wire, PROTOCOL};
use hl_ws::storage::Directory;

use super::super::{Page, View};
use super::{directory, settings, Catalogue, Cleanup, Inspection, PendingInspection, Shared, Shelf, Surfaces};

/// The style class the fake surface carries, so a test can tell an extension's
/// own page from the settings page beside it.
const SURFACE: &str = "hl-test-surface";

/// Every scenario runs inside one test, on the binary's toolkit thread.
///
/// GTK belongs to whichever thread entered it and libtest gives every `#[test]`
/// its own, so the scenarios are handed to `test_support`, which owns the one
/// thread in this process that entered GTK. Entering it here instead is what
/// used to make this test either SIGSEGV on a display-less host or panic beside
/// the extension-page test on a host with a display; `test_support` documents
/// both mechanisms.
#[test]
fn a_workspaces_extensions_are_on_its_sidebar_and_hear_what_is_clicked() {
    let ran = crate::test_support::on_the_toolkit_thread(|| {
        the_sidebar_lists_exactly_what_the_workspace_recorded();
        selecting_an_extension_shows_the_surface_it_draws();
        the_settings_page_says_where_an_extension_stands();
        a_live_host_fault_reaches_central_settings_and_can_retry();
        the_settings_actions_drive_the_installation();
        lifecycle_actions_share_keyboard_and_semantic_focus();
        native_extension_cards_are_semantic_and_actionable();
        removing_an_extension_takes_its_pages_with_it();
        failed_removal_keeps_a_disabled_record_and_offers_retry();
        management_extension_reconciles_native_fallback_pages();
        docker_hub_references_are_explained_and_validated_before_acquisition();
        an_image_is_read_before_anybody_is_asked();
        an_existing_name_is_an_explicit_update_with_a_capability_delta();
        a_stale_update_failure_keeps_the_installed_extension_and_can_be_retried();
        remote_image_progress_precedes_the_consent_prompt();
        cancelling_an_acquisition_rejects_a_late_ready_result_and_offers_retry();
        a_failed_registry_read_can_be_retried_without_duplicate_work();
        a_declined_image_records_nothing();
        a_click_on_a_rendered_button_reaches_the_extension();
        panes::reading_a_pane_hands_back_what_was_written_to_it();
        panes::native_workspace_semantics_cross_the_terminal_request_bridge();
        panes::a_pane_read_never_answers_with_more_than_it_was_allowed();
        panes::dividing_a_pane_produces_a_slot_that_can_be_addressed();
        panes::closing_a_pane_by_slot_removes_that_one_and_leaves_the_rest();
        panes::a_pane_can_hold_an_extensions_interface_beside_a_shell();
        panes::providers_are_advertised_only_with_a_readable_projection();
        panes::a_pane_chooser_switches_to_a_provider_and_back_to_its_shell();
        panes::each_split_chooser_switches_its_own_pane_without_stealing_terminal_focus();
        panes::an_existing_pane_chooser_discovers_a_later_provider();
        panes::pane_chooser_groups_and_filters_many_extension_views();
        panes::disabling_an_extension_tombstones_and_recovers_its_surface_pane();
        panes::removing_an_extension_tombstones_without_displacing_its_shell();
        panes::every_split_leaf_owns_its_chooser_and_topology_is_nested();
        panes::two_same_extension_panes_render_independently_by_slot();
        panes::a_failed_interface_split_leaves_its_surface_where_it_was();
        panes::a_restored_surface_without_its_extension_is_frozen_rather_than_a_shell();
    });
    if !ran {
        eprintln!("skipped: no display connection, so the extension shelf cannot be rendered");
    }
}

#[cfg(feature = "mcp-e2e")]
#[test]
fn a_real_mcp_client_discovers_native_terminal_and_rust_extension_surfaces() {
    let ran = crate::test_support::on_the_toolkit_thread(|| panes::mcp_socket_changes_native_ui());
    assert!(ran, "the explicit MCP integration target requires an X display");
}

fn native_extension_cards_are_semantic_and_actionable() {
    use super::super::semantic::{Action, ActionKind};
    let fixture = Fixture::new(&[("semantic", false)]);
    let snapshot = fixture.view.semantic_snapshot();
    let card = snapshot
        .root
        .children
        .iter()
        .find(|node| node.role == "group" && node.label.as_deref() == Some("semantic"))
        .expect("the visible lifecycle card is represented");
    assert_eq!(card.value.as_deref(), Some("version 1.0.0; disabled"));
    let grants = snapshot
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Granted capabilities"))
        .expect("the consented authority is visible to agents");
    assert!(grants.value.as_deref().is_some_and(|value| {
        value.contains("interface") && value.contains("container-read")
    }));
    let enable = snapshot
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Enable"))
        .expect("the disabled extension's owner registered Enable");
    fixture
        .view
        .semantic_action(&Action {
            revision: snapshot.revision,
            node: enable.id,
            action: ActionKind::Invoke,
            value: None,
        })
        .unwrap();
    assert_eq!(fixture.stage("semantic"), Stage::Duty);
    let refreshed = fixture.view.semantic_snapshot();
    assert!(refreshed.revision > snapshot.revision);
    assert!(refreshed
        .root
        .children
        .iter()
        .any(|node| node.label.as_deref() == Some("Disable")));
    assert!(refreshed
        .root
        .children
        .iter()
        .any(|node| node.label.as_deref() == Some("Read manifest")));

    let empty = Fixture::new(&[]).view.semantic_snapshot();
    assert!(empty.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Installed extensions") && node.value.as_deref() == Some("None installed")
    }));
}

/// One shell, one roster, and the shelf between them.
struct Fixture {
    _storage: tempfile::TempDir,
    view: Rc<View>,
    roster: Shared,
    shelf: Rc<Shelf>,
    _catalogue: Rc<Catalogue>,
}

impl Fixture {
    /// A shelf over a roster holding `recorded`, on a shell with the fixed pages.
    fn new(recorded: &[(&str, bool)]) -> Self {
        let cleanup: Cleanup = Rc::new(|_| {
            let (sent, received) = std::sync::mpsc::channel();
            let _ = sent.send(Ok(()));
            received
        });
        Self::with_cleanup(recorded, cleanup)
    }

    fn with_cleanup(recorded: &[(&str, bool)], cleanup: Cleanup) -> Self {
        let storage = tempfile::tempdir().expect("temporary directory");
        let roster = Rc::new(RefCell::new(
            Roster::open(Directory::open(storage.path()).expect("storage")).expect("roster"),
        ));
        for (name, enabled) in recorded {
            record(&roster, name, *enabled);
        }
        let view = Rc::new(View::new([
            (Page::Overview, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
            (Page::Extensions, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
        ]));
        let surfaces: Surfaces = Rc::new(|_| {
            let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
            widget.add_css_class(SURFACE);
            widget.upcast()
        });
        let shelf = Shelf::with_cleanup(&view, &roster, surfaces, Rc::new(|_| {}), Rc::new(|_| {}), cleanup);
        shelf.install();
        let inspection: Inspection = Rc::new(|_| PendingInspection::detached(std::sync::mpsc::channel().1));
        let catalogue = Catalogue::new(&shelf, inspection);
        view.page(Page::Extensions.title())
            .and_downcast::<gtk::Box>()
            .expect("extensions page")
            .append(catalogue.widget());
        Self {
            _storage: storage,
            view,
            roster,
            shelf,
            _catalogue: catalogue,
        }
    }

    /// Where one extension stands, as the roster now says.
    fn stage(&self, name: &str) -> Stage {
        self.roster.borrow().stage(&named(name))
    }

    /// The first widget on a page carrying a style class.
    fn tagged(&self, page: &str, class: &str) -> Option<gtk::Widget> {
        let page = self.view.page(page)?;
        descendants(&page)
            .into_iter()
            .find(|widget| widget.has_css_class(class))
    }

    /// Clicks the action on a settings page carrying a style class.
    fn act(&self, name: &str, class: &str) {
        self.extension_tagged(name, class)
            .unwrap_or_else(|| panic!("{class} is offered on {name}'s card"))
            .downcast::<gtk::Button>()
            .expect("an action is a button")
            .emit_clicked();
    }

    fn extension_tagged(&self, name: &str, class: &str) -> Option<gtk::Widget> {
        let page = self.view.page(Page::Extensions.title())?;
        descendants(&page)
            .into_iter()
            .filter(|widget| widget.has_css_class(settings::CARD))
            .find(|card| {
                descendants(card).iter().any(|widget| {
                    widget.has_css_class("dhead")
                        && widget
                            .downcast_ref::<gtk::Label>()
                            .is_some_and(|label| label.text() == name)
                })
            })
            .and_then(|card| {
                descendants(&card)
                    .into_iter()
                    .find(|widget| widget.has_css_class(class))
            })
    }
}

/// Writes one record straight through the roster, which is what an install did.
fn record(roster: &Shared, name: &str, enabled: bool) {
    let manifest = manifest(name);
    let mut held = roster.borrow_mut();
    held.register(&manifest, "sha256:aaaa", &manifest.capabilities, 1)
        .expect("registered");
    if enabled {
        held.enable(&manifest.name).expect("enabled");
    }
}

fn named(name: &str) -> ExtensionName {
    ExtensionName::new(name).expect("name")
}

fn manifest(name: &str) -> Manifest {
    Manifest {
        name: named(name),
        display_name: name.to_owned(),
        version: "1.0.0".to_owned(),
        protocol: PROTOCOL,
        capabilities: Grant::new([Capability::Interface, Capability::ContainerRead]),
        entrypoint: None,
        activation: hl_extension::Activation::default(),
        interface: None,
        pane_providers: Vec::new(),
        resources: hl_extension::Resources::default(),
        filesystem_roots: Vec::new(),
    }
}

/// Every widget under one, parents before children.
fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut index = 0;
    while index < found.len() {
        let mut cursor = found[index].first_child();
        while let Some(child) = cursor {
            cursor = child.next_sibling();
            found.push(child);
        }
        index += 1;
    }
    found
}

fn focus_chain(window: &gtk::Window) -> Vec<gtk::Widget> {
    gtk::prelude::RootExt::set_focus(window, gtk::Widget::NONE);
    let mut found = Vec::new();
    for _ in 0..64 {
        if !window.child_focus(gtk::DirectionType::TabForward) {
            break;
        }
        let Some(focus) = gtk::prelude::RootExt::focus(window) else { break };
        if found.iter().any(|seen| seen == &focus) {
            break;
        }
        found.push(focus);
    }
    found
}

fn has_focusable_ancestor(widget: &gtk::Widget) -> bool {
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if widget.is_focusable() {
            return true;
        }
        parent = widget.parent();
    }
    false
}

/// Waits for something another thread reaches on its own schedule.
fn until(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn until_gui(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        while gtk::glib::MainContext::default().iteration(false) {}
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn the_sidebar_lists_exactly_what_the_workspace_recorded() {
    let fixture = Fixture::new(&[("alpha", false), ("zulu", true)]);

    let listed = fixture.view.entries();

    assert!(listed.contains(&"alpha".to_owned()), "got {listed:?}");
    assert!(listed.contains(&"zulu".to_owned()), "got {listed:?}");
    assert!(
        !listed.iter().any(|entry| entry.ends_with(" settings")),
        "lifecycle cards do not duplicate the sidebar: {listed:?}"
    );
    assert!(
        !listed.contains(&"other".to_owned()),
        "only this workspace's extensions are listed"
    );
    assert_eq!(
        listed.iter().filter(|entry| entry.as_str() == "alpha").count(),
        1,
        "one entry per extension"
    );
}

fn selecting_an_extension_shows_the_surface_it_draws() {
    let fixture = Fixture::new(&[("alpha", true)]);

    fixture.view.select_name("alpha");

    assert_eq!(fixture.view.shown().as_deref(), Some("alpha"));
    assert!(
        fixture.tagged("alpha", SURFACE).is_some(),
        "the extension's own surface is the page"
    );
    assert_eq!(fixture.view.entries(), ["Overview", "Extensions", "alpha"]);
}

fn the_settings_page_says_where_an_extension_stands() {
    let fixture = Fixture::new(&[("alpha", true), ("zulu", false)]);

    let duty = fixture
        .extension_tagged("alpha", settings::STANDING)
        .and_downcast::<gtk::Label>()
        .expect("a standing");
    let standby = fixture
        .extension_tagged("zulu", settings::STANDING)
        .and_downcast::<gtk::Label>()
        .expect("a standing");

    assert_eq!(duty.text(), "enabled");
    assert_eq!(standby.text(), "disabled");
    assert!(
        fixture.extension_tagged("alpha", settings::DISABLE).is_some(),
        "an enabled extension is offered a disable"
    );
    assert!(
        fixture.extension_tagged("zulu", settings::ENABLE).is_some(),
        "a disabled extension is offered an enable"
    );
}

fn a_live_host_fault_reaches_central_settings_and_can_retry() {
    let fixture = Fixture::new(&[("alpha", true)]);

    fixture.shelf.fault(&named("alpha"), 5);

    assert_eq!(fixture.stage("alpha"), Stage::Fault { restarts: 5 });
    let standing = fixture
        .extension_tagged("alpha", settings::STANDING)
        .and_downcast::<gtk::Label>()
        .expect("central settings standing");
    assert_eq!(standing.text(), "faulted after 5 restarts");
    assert!(fixture.extension_tagged("alpha", settings::RETRY).is_some());

    fixture.act("alpha", settings::RETRY);

    assert_eq!(fixture.stage("alpha"), Stage::Duty);
    assert!(fixture.view.holds("alpha"), "retry remounts the extension surface");
    assert!(
        fixture.extension_tagged("alpha", settings::REMOVE).is_some(),
        "retry did not remove it"
    );
}

fn the_settings_actions_drive_the_installation() {
    let fixture = Fixture::new(&[("alpha", false)]);
    assert_eq!(fixture.stage("alpha"), Stage::Standby);

    fixture.act("alpha", settings::ENABLE);
    assert_eq!(fixture.stage("alpha"), Stage::Duty, "the policy was told");

    fixture.act("alpha", settings::DISABLE);
    assert_eq!(fixture.stage("alpha"), Stage::Standby, "and told again");
    assert!(
        fixture.extension_tagged("alpha", settings::ENABLE).is_some(),
        "the page was rebuilt from what the policy now says"
    );
}

fn lifecycle_actions_share_keyboard_and_semantic_focus() {
    use super::super::semantic::{Action, ActionKind, Refusal};
    let fixture = Fixture::new(&[("alpha", false)]);
    fixture.view.select_name(Page::Extensions.title());
    let window = gtk::Window::builder()
        .default_width(900)
        .default_height(700)
        .child(&fixture.view.widget)
        .build();
    window.present();
    while gtk::glib::MainContext::default().iteration(false) {}
    let focusable: Vec<_> = descendants(fixture.view.widget.upcast_ref())
        .into_iter()
        .filter(|widget| {
            (widget.is::<gtk::Entry>() || widget.is::<gtk::Button>() || widget.is::<gtk::ToggleButton>())
                && widget.is_focusable()
                && widget.is_sensitive()
                && widget.is_visible()
                && !has_focusable_ancestor(widget)
        })
        .collect();
    let traversed = focus_chain(&window);
    assert!(
        focusable
            .iter()
            .all(|widget| traversed.iter().any(|focused| focused == widget)),
        "Tab reaches every visible enabled catalogue/lifecycle control"
    );

    let initial = fixture.view.semantic_snapshot();
    let enable = initial
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Enable"))
        .expect("visible Enable semantics");
    assert!(enable.actions.contains(&ActionKind::Focus));
    fixture
        .view
        .semantic_action(&Action {
            revision: initial.revision,
            node: enable.id,
            action: ActionKind::Focus,
            value: None,
        })
        .unwrap();
    assert_eq!(
        gtk::prelude::RootExt::focus(&window)
            .and_downcast::<gtk::Button>()
            .and_then(|button| button.label()),
        Some("Enable".into())
    );

    let removal = fixture.view.semantic_snapshot();
    let remove = removal
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Remove"))
        .unwrap();
    let confirm = removal
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Confirm removal"))
        .unwrap();
    assert!(confirm.disabled, "hidden confirmation is not focusable");
    assert!(confirm.actions.is_empty(), "disabled native controls advertise no actions");
    assert!(matches!(
        fixture.view.semantic_action(&Action {
            revision: removal.revision,
            node: confirm.id,
            action: ActionKind::Focus,
            value: None,
        }),
        Err(Refusal::Disabled(id)) if id == confirm.id
    ));
    fixture
        .view
        .semantic_action(&Action {
            revision: removal.revision,
            node: remove.id,
            action: ActionKind::Invoke,
            value: None,
        })
        .unwrap();
    let asking = fixture.view.semantic_snapshot();
    let confirm = asking
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Confirm removal"))
        .unwrap();
    assert!(!confirm.disabled);
    fixture
        .view
        .semantic_action(&Action {
            revision: asking.revision,
            node: confirm.id,
            action: ActionKind::Focus,
            value: None,
        })
        .unwrap();
    assert_eq!(
        gtk::prelude::RootExt::focus(&window)
            .and_downcast::<gtk::Button>()
            .and_then(|button| button.label()),
        Some("Confirm removal".into()),
        "semantic Focus follows the currently visible confirmation"
    );
    window.close();
}

fn removing_an_extension_takes_its_pages_with_it() {
    let fixture = Fixture::new(&[("alpha", true)]);

    fixture.act("alpha", settings::REMOVE);
    assert_eq!(
        fixture.stage("alpha"),
        Stage::Duty,
        "asking for confirmation changes nothing"
    );
    fixture.act("alpha", settings::CONFIRM_REMOVE);
    assert!(until_gui(|| fixture.stage("alpha") == Stage::Vacancy));

    assert_eq!(fixture.stage("alpha"), Stage::Vacancy, "the record is forgotten");
    assert!(!fixture.view.holds("alpha"), "its surface is off the shell");
    assert!(
        fixture.extension_tagged("alpha", settings::STANDING).is_none(),
        "its lifecycle card is gone"
    );
    assert!(
        !fixture.view.entries().contains(&"alpha".to_owned()),
        "and its sidebar entry is gone"
    );
}

fn failed_removal_keeps_a_disabled_record_and_offers_retry() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&attempts);
    let cleanup: Cleanup = Rc::new(move |_| {
        counted.fetch_add(1, Ordering::Release);
        let (sent, received) = std::sync::mpsc::channel();
        let _ = sent.send(Err("foreign container occupies the managed name".to_owned()));
        received
    });
    let fixture = Fixture::with_cleanup(&[("alpha", true)], cleanup);

    fixture.act("alpha", settings::REMOVE);
    let confirmation = fixture.view.semantic_snapshot();
    let confirm = confirmation
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Confirm removal"))
        .expect("the confirmation is represented semantically");
    assert!(confirm.destructive, "only the final removal authority is destructive");
    assert!(!confirm.disabled, "the final authority is enabled after the first step");
    assert!(confirmation.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Lifecycle notice")
            && node
                .value
                .as_deref()
                .is_some_and(|value| value.contains("managed sidecar?"))
    }));
    fixture.act("alpha", settings::CANCEL_REMOVE);
    assert_eq!(
        fixture.stage("alpha"),
        Stage::Duty,
        "cancel leaves runtime and record alone"
    );
    assert_eq!(attempts.load(Ordering::Acquire), 0);
    let cancelled = fixture.view.semantic_snapshot();
    assert!(cancelled.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Lifecycle notice")
            && node.value.as_deref() == Some("Removal cancelled; nothing changed")
    }));

    fixture.act("alpha", settings::REMOVE);
    fixture.act("alpha", settings::CONFIRM_REMOVE);
    assert!(until_gui(|| {
        fixture
            .extension_tagged("alpha", settings::REFUSAL)
            .and_downcast::<gtk::Label>()
            .is_some_and(|label| label.text().contains("remains installed and disabled"))
    }));
    assert_eq!(fixture.stage("alpha"), Stage::Standby);
    assert_eq!(attempts.load(Ordering::Acquire), 1);
    let failed = fixture.view.semantic_snapshot();
    assert!(failed.root.children.iter().any(|node| {
        node.label.as_deref() == Some("alpha") && node.value.as_deref() == Some("disabled · removal failed")
    }));
    assert!(failed.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Lifecycle notice")
            && node
                .value
                .as_deref()
                .is_some_and(|value| value.contains("foreign container"))
    }));
    assert!(
        fixture.extension_tagged("alpha", settings::CONFIRM_REMOVE).is_some(),
        "the same confirmed action becomes an explicit cleanup retry"
    );
}

fn management_extension_reconciles_native_fallback_pages() {
    let storage = tempfile::tempdir().expect("temporary directory");
    let roster = Rc::new(RefCell::new(
        Roster::open(Directory::open(storage.path()).expect("storage")).expect("roster"),
    ));
    record(&roster, super::MANAGEMENT_EXTENSION, true);
    let overview = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let containers = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let view = Rc::new(View::new([
        (Page::Settings, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
        (Page::Extensions, gtk::Box::new(gtk::Orientation::Vertical, 0).upcast()),
    ]));
    let fallback = [
        (Page::Overview, overview.upcast::<gtk::Widget>()),
        (Page::Containers, containers.upcast()),
    ];
    for (page, widget) in &fallback {
        view.attach(page.title(), widget);
    }
    let weak = Rc::downgrade(&view);
    let reconcile = Rc::new(move |managed: bool| {
        let Some(view) = weak.upgrade() else { return };
        for (page, widget) in &fallback {
            if managed {
                view.detach(page.title());
            } else if !view.holds(page.title()) {
                view.attach(page.title(), widget);
            }
        }
    });
    let surfaces: Surfaces = Rc::new(|_| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
    let withdrawals = Rc::new(Cell::new(0));
    let counted = Rc::clone(&withdrawals);
    let withdraw = Rc::new(move |name: &ExtensionName| {
        if name.as_str() == super::MANAGEMENT_EXTENSION {
            counted.set(counted.get() + 1);
        }
    });
    let shelf = Shelf::with_lifecycle(&view, &roster, surfaces, reconcile, withdraw);
    shelf.install();

    let assert_pages = |managed: bool| {
        let entries = view.entries();
        for title in [Page::Overview.title(), Page::Containers.title()] {
            assert_eq!(
                entries.iter().filter(|entry| entry.as_str() == title).count(),
                usize::from(!managed),
                "{title} ownership must follow Duty without duplicates: {entries:?}"
            );
        }
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.as_str() == super::MANAGEMENT_EXTENSION)
                .count(),
            1,
            "the extension surface itself is never duplicated: {entries:?}"
        );
    };
    assert_pages(true);

    let before = withdrawals.get();
    roster
        .borrow_mut()
        .disable(&named(super::MANAGEMENT_EXTENSION))
        .expect("disabled");
    shelf.refresh(&named(super::MANAGEMENT_EXTENSION));
    assert_pages(false);
    assert!(
        withdrawals.get() > before,
        "disable withdraws any provider panes before fallback returns"
    );

    roster
        .borrow_mut()
        .enable(&named(super::MANAGEMENT_EXTENSION))
        .expect("enabled");
    shelf.refresh(&named(super::MANAGEMENT_EXTENSION));
    assert_pages(true);

    let before = withdrawals.get();
    roster
        .borrow_mut()
        .fault(&named(super::MANAGEMENT_EXTENSION), 3)
        .expect("fault recorded");
    shelf.refresh(&named(super::MANAGEMENT_EXTENSION));
    assert_pages(false);
    assert!(
        withdrawals.get() > before,
        "fault withdraws provider panes before fallback returns"
    );

    let before = withdrawals.get();
    roster
        .borrow_mut()
        .retry(&named(super::MANAGEMENT_EXTENSION))
        .expect("retry returns to duty");
    shelf.refresh(&named(super::MANAGEMENT_EXTENSION));
    assert_pages(true);
    assert!(
        withdrawals.get() > before,
        "retry replaces the faulted surface before taking ownership"
    );

    let before = withdrawals.get();
    roster
        .borrow_mut()
        .remove(&named(super::MANAGEMENT_EXTENSION))
        .expect("removed management extension");
    shelf.refresh(&named(super::MANAGEMENT_EXTENSION));
    assert_eq!(
        roster.borrow().stage(&named(super::MANAGEMENT_EXTENSION)),
        Stage::Vacancy
    );
    assert!(view.holds(Page::Overview.title()));
    assert!(view.holds(Page::Containers.title()));
    assert_eq!(
        view.entries()
            .iter()
            .filter(|entry| entry.as_str() == Page::Overview.title())
            .count(),
        1,
        "removal restores one fallback page"
    );
    assert!(withdrawals.get() > before, "removal withdraws provider panes");

    record(&roster, "containers", true);
    let legacy = roster
        .borrow()
        .entries()
        .into_iter()
        .find(|entry| entry.name.as_str() == "containers")
        .expect("legacy reference extension");
    shelf.mount(&legacy);
    assert!(view.holds(Page::Overview.title()));
    assert!(
        view.holds(Page::Containers.title()),
        "legacy containers cannot suppress views it does not replace"
    );
}

/// A catalogue whose inspection answers with `answer`, with nothing installed.
fn catalogue(fixture: &Fixture, answer: Result<Candidate, String>) -> Rc<Catalogue> {
    let held = Mutex::new(Some(answer));
    let inspection: Inspection = Rc::new(move |_| {
        let (answered, answer) = std::sync::mpsc::channel();
        let taken = held.lock().expect("answer").take();
        if let Some(taken) = taken {
            let event = match taken {
                Ok(candidate) => Acquisition::Ready(candidate),
                Err(reason) => Acquisition::Failed(reason),
            };
            let _ = answered.send(event);
        }
        PendingInspection::detached(answer)
    });
    Catalogue::new(&fixture.shelf, inspection)
}

/// Types an image reference into the page's own field, which is where the
/// page reads it from.
fn typed(page: &Rc<Catalogue>, reference: &str) {
    descendants(page.widget().clone().upcast_ref())
        .into_iter()
        .find(|widget| widget.has_css_class(directory::REFERENCE))
        .and_downcast::<gtk::Entry>()
        .expect("a field to type the image into")
        .set_text(reference);
}

fn inspect_action(page: &Rc<Catalogue>) -> gtk::Button {
    descendants(page.widget().clone().upcast_ref())
        .into_iter()
        .find(|widget| widget.has_css_class(directory::INSPECT))
        .and_downcast::<gtk::Button>()
        .expect("an image inspection action")
}

fn candidate() -> Candidate {
    Candidate {
        reference: "sample:1".to_owned(),
        digest: "sha256:bbbb".to_owned(),
        manifest: manifest("sample"),
    }
}

fn docker_hub_references_are_explained_and_validated_before_acquisition() {
    let fixture = Fixture::new(&[]);
    let attempts = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&attempts);
    let inspection: Inspection = Rc::new(move |reference| {
        recorded.borrow_mut().push(reference.to_owned());
        PendingInspection::detached(std::sync::mpsc::channel().1)
    });
    let page = Catalogue::new(&fixture.shelf, inspection);
    let copy: Vec<_> = descendants(page.widget().upcast_ref())
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
        .map(|label| label.text().to_string())
        .collect();
    assert!(copy.iter().any(|line| line.contains("Docker Hub examples")));

    typed(&page, "not a reference with spaces");
    page.inspect();
    assert!(attempts.borrow().is_empty(), "invalid input never starts acquisition");
    assert!(page.notice().contains("not a valid image reference"));

    typed(&page, "alpine:3.20");
    page.inspect();
    assert_eq!(attempts.borrow().as_slice(), ["docker.io/library/alpine:3.20"]);
}

fn update_candidate(digest: &str, version: &str) -> Candidate {
    let mut manifest = manifest("sample");
    manifest.version = version.to_owned();
    manifest.capabilities = Grant::new([Capability::Interface, Capability::ContainerControl]);
    Candidate {
        reference: format!("sample:{version}"),
        digest: digest.to_owned(),
        manifest,
    }
}

fn an_image_is_read_before_anybody_is_asked() {
    let fixture = Fixture::new(&[]);
    let page = catalogue(&fixture, Ok(candidate()));
    typed(&page, "sample:1");

    page.inspect();
    assert!(page.poll(), "the inspection came back");

    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "reading an image records nothing on its own"
    );
    assert!(
        page.notice().contains("asks for"),
        "what it asks for is put to a person, got {:?}",
        page.notice()
    );
    let proposal: Vec<_> = descendants(page.widget().upcast_ref())
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
        .map(|label| label.text().to_string())
        .collect();
    assert!(proposal.contains(&"Image: sample:1".to_owned()));
    assert!(proposal.contains(&"Digest: sha256:bbbb".to_owned()));
    let proposed = fixture.view.semantic_snapshot();
    for label in ["Install", "Cancel"] {
        let action = proposed
            .root
            .children
            .iter()
            .find(|node| node.label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("proposal lacks {label} semantics"));
        assert!(
            action.actions.contains(&super::super::semantic::ActionKind::Focus),
            "{label} is focusable without invoking consent"
        );
    }

    page.consent();

    let entries = fixture.roster.borrow().entries();
    assert_eq!(entries.len(), 1, "consent is what records the grant");
    assert_eq!(entries[0].image_digest, "sha256:bbbb");
    assert!(entries[0].granted.holds(Capability::Interface));
    assert_eq!(entries[0].stage, Stage::Standby, "an install starts off duty");
    assert!(page.notice().contains("sample:1 at sha256:bbbb"));
    assert!(
        fixture.view.holds("sample"),
        "and it is on the sidebar without a restart"
    );
    assert!(!fixture.view.entries().iter().any(|entry| entry.ends_with(" settings")));
    assert!(
        descendants(page.widget().upcast_ref())
            .iter()
            .any(|widget| widget.has_css_class(settings::STANDING)),
        "the central catalogue gained the lifecycle card"
    );
}

fn an_existing_name_is_an_explicit_update_with_a_capability_delta() {
    let fixture = Fixture::new(&[("sample", true)]);
    let old_surface = fixture.view.page("sample").expect("installed surface");
    let page = catalogue(&fixture, Ok(update_candidate("sha256:cccc", "2.0.0")));
    typed(&page, "sample:2");
    page.inspect();
    assert!(page.poll());

    let proposal = descendants(page.widget().upcast_ref());
    let labels: Vec<_> = proposal
        .iter()
        .filter_map(|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .map(|label| label.text().to_string())
        })
        .collect();
    assert!(labels
        .iter()
        .any(|line| line.contains("installed") && line.contains("sha256:aaaa")));
    assert!(labels
        .iter()
        .any(|line| line.contains("candidate  2.0.0") && line.contains("sha256:cccc")));
    assert!(
        labels.iter().any(|line| line == "+ container-control"),
        "new authority is called out explicitly: {labels:?}"
    );
    assert!(
        labels.iter().any(|line| line == "− container-read"),
        "authority the candidate dropped is called out explicitly: {labels:?}"
    );
    assert_eq!(
        fixture.view.page("sample").as_ref(),
        Some(&old_surface),
        "inspection and prompt leave the old extension live"
    );

    page.consent();
    let entry = fixture
        .roster
        .borrow()
        .entries()
        .into_iter()
        .find(|entry| entry.name.as_str() == "sample")
        .expect("updated record");
    assert_eq!(entry.image_digest, "sha256:cccc");
    assert_eq!(entry.version, "2.0.0");
    assert!(entry.granted.holds(Capability::ContainerControl));
    assert!(!entry.granted.holds(Capability::ContainerRead));
    assert!(fixture.view.holds("sample"));
    assert!(
        descendants(page.widget().upcast_ref()).iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == "version  ·  2.0.0")
        }),
        "the lifecycle card identifies the active version"
    );
}

fn a_stale_update_failure_keeps_the_installed_extension_and_can_be_retried() {
    let fixture = Fixture::new(&[("sample", true)]);
    let page = catalogue(&fixture, Ok(update_candidate("sha256:cccc", "2.0.0")));
    typed(&page, "sample:2");
    page.inspect();
    assert!(page.poll());
    let old_surface = fixture.view.page("sample").expect("old surface");

    let winner = update_candidate("sha256:dddd", "1.5.0");
    let prepared = fixture
        .roster
        .borrow()
        .prepare_update(&winner.manifest, &winner.digest)
        .expect("competing update");
    fixture
        .roster
        .borrow_mut()
        .commit_update(prepared, &Grant::new([Capability::ContainerControl]), 2)
        .expect("competing update commits");

    page.consent();
    assert!(
        page.notice().contains("unchanged"),
        "failure is visible: {}",
        page.notice()
    );
    assert_eq!(
        fixture.view.page("sample").as_ref(),
        Some(&old_surface),
        "a refused replacement does not rebuild the running surface"
    );
    assert!(
        descendants(page.widget().upcast_ref())
            .iter()
            .any(|widget| widget.has_css_class(directory::CONSENT)),
        "the accepted proposal remains available for an explicit retry"
    );
    assert_eq!(
        fixture
            .roster
            .borrow()
            .entries()
            .into_iter()
            .find(|entry| entry.name.as_str() == "sample")
            .expect("winner remains")
            .image_digest,
        "sha256:dddd"
    );
}

fn a_declined_image_records_nothing() {
    let fixture = Fixture::new(&[]);
    let page = catalogue(&fixture, Ok(candidate()));
    typed(&page, "sample:1");
    page.inspect();
    assert!(page.poll(), "the inspection came back");

    page.decline();

    assert!(fixture.roster.borrow().entries().is_empty(), "nothing was recorded");
    assert!(!fixture.view.holds("sample"), "and nothing reached the sidebar");
    page.consent();
    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "a declined candidate cannot be installed afterwards"
    );
}

fn remote_image_progress_precedes_the_consent_prompt() {
    let fixture = Fixture::new(&[]);
    let events = Mutex::new(Some(vec![
        Acquisition::Inspecting,
        Acquisition::Pulling {
            status: "Pulling from team/tool".to_owned(),
            id: Some("team/tool:latest".to_owned()),
            current: Some(25),
            total: Some(100),
        },
        Acquisition::ReadingManifest,
        Acquisition::Ready(candidate()),
    ]));
    let inspection: Inspection = Rc::new(move |_| {
        let (sent, received) = std::sync::mpsc::channel();
        if let Some(events) = events.lock().expect("events").take() {
            for event in events {
                sent.send(event).expect("catalogue is listening");
            }
        }
        PendingInspection::detached(received)
    });
    let page = Catalogue::new(&fixture.shelf, inspection);
    typed(&page, "team/tool:latest");
    page.inspect();

    assert!(page.poll());
    assert_eq!(page.notice(), "checking local images");
    let checking = fixture.view.semantic_snapshot();
    let progress = checking
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Image acquisition progress"))
        .expect("semantic clients see acquisition progress");
    assert_eq!(progress.value.as_deref(), Some("checking local images"));
    assert!(checking.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Cancel download")
            && node.actions.contains(&super::super::semantic::ActionKind::Invoke)
    }));
    assert!(page.poll());
    assert!(page.notice().contains("Pulling from team/tool"));
    let pulling = fixture.view.semantic_snapshot();
    assert!(pulling.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Image acquisition progress")
            && node
                .value
                .as_deref()
                .is_some_and(|value| value.contains("25%; 25 of 100 bytes"))
    }));
    assert!(fixture.roster.borrow().entries().is_empty(), "progress is not consent");
    assert!(page.poll());
    assert_eq!(page.notice(), "reading extension manifest");
    assert!(page.poll());
    assert!(page.notice().contains("asks for"));
    let ready = fixture.view.semantic_snapshot();
    assert!(!ready
        .root
        .children
        .iter()
        .any(|node| node.label.as_deref() == Some("Cancel download")));
    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "a ready image still awaits consent"
    );
}

fn cancelling_an_acquisition_rejects_a_late_ready_result_and_offers_retry() {
    let fixture = Fixture::new(&[]);
    let sender = Arc::new(Mutex::new(None));
    let held_sender = Arc::clone(&sender);
    let cancellation = Arc::new(Mutex::new(None));
    let held_cancellation = Arc::clone(&cancellation);
    let inspection: Inspection = Rc::new(move |_| {
        let (sent, events) = std::sync::mpsc::channel();
        held_sender.lock().expect("sender").replace(sent);
        let token = hl::extension::Cancellation::default();
        held_cancellation.lock().expect("cancellation").replace(token.clone());
        PendingInspection {
            events,
            cancellation: token,
        }
    });
    let page = Catalogue::new(&fixture.shelf, inspection);
    typed(&page, "team/tool:latest");
    page.inspect();
    page.cancel();

    let cancelling = fixture.view.semantic_snapshot();
    let cancel = cancelling
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Cancel download"))
        .expect("the in-flight cancellation remains observable");
    assert_eq!(cancel.value.as_deref(), Some("Cancellation requested"));
    assert!(cancel.disabled, "an agent cannot submit duplicate cancellation");
    assert!(matches!(
        fixture.view.semantic_action(&super::super::semantic::Action {
            revision: cancelling.revision,
            node: cancel.id,
            action: super::super::semantic::ActionKind::Invoke,
            value: None,
        }),
        Err(super::super::semantic::Refusal::Disabled(id)) if id == cancel.id
    ));

    assert!(
        cancellation
            .lock()
            .expect("cancellation")
            .as_ref()
            .is_some_and(hl::extension::Cancellation::is_cancelled),
        "the UI reaches the worker's cancellation authority"
    );
    sender
        .lock()
        .expect("sender")
        .as_ref()
        .expect("pending sender")
        .send(Acquisition::Ready(candidate()))
        .expect("late worker result");
    assert!(page.poll());
    assert!(fixture.roster.borrow().entries().is_empty());
    assert!(page.notice().contains("nothing was installed"));
    let cancelled = fixture.view.semantic_snapshot();
    assert!(cancelled.root.children.iter().any(|node| {
        node.label.as_deref() == Some("Read manifest")
            && node.value.as_deref() == Some("Retry acquisition")
            && !node.disabled
    }));
    page.consent();
    assert!(
        fixture.roster.borrow().entries().is_empty(),
        "late readiness cannot install"
    );
    assert!(
        inspect_action(&page).is_sensitive(),
        "retry is offered after acknowledgement"
    );
}

fn a_failed_registry_read_can_be_retried_without_duplicate_work() {
    let fixture = Fixture::new(&[]);
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let answers = Mutex::new(vec![
        Acquisition::Ready(candidate()),
        Acquisition::Failed("registry temporarily unavailable".to_owned()),
    ]);
    let recorded = Arc::clone(&attempts);
    let inspection: Inspection = Rc::new(move |reference| {
        recorded.lock().expect("attempts").push(reference.to_owned());
        let (sent, received) = std::sync::mpsc::channel();
        if let Some(answer) = answers.lock().expect("answers").pop() {
            sent.send(answer).expect("catalogue is listening");
        }
        PendingInspection::detached(received)
    });
    let page = Catalogue::new(&fixture.shelf, inspection);
    typed(&page, "team/tool:latest");

    page.inspect();
    let action = inspect_action(&page);
    assert!(
        !action.is_sensitive(),
        "a second pull cannot be started while one is pending"
    );
    assert_eq!(action.label().as_deref(), Some("Reading…"));
    assert!(page.poll());
    assert!(action.is_sensitive());
    assert_eq!(action.label().as_deref(), Some("Retry"));
    assert!(page.notice().contains("temporarily unavailable"));

    page.inspect();
    assert!(!action.is_sensitive());
    assert!(page.poll());
    assert_eq!(action.label().as_deref(), Some("Read another image"));
    page.consent();
    assert!(
        fixture.view.holds("sample"),
        "retry reaches the ordinary consent lifecycle"
    );
    assert_eq!(
        *attempts.lock().expect("attempts"),
        ["docker.io/team/tool:latest", "docker.io/team/tool:latest"],
        "only the two explicit attempts reached the registry"
    );
}

/// What the fake extension heard, in order.
type Heard = Arc<Mutex<Vec<String>>>;

/// A supply with no container daemon: `ensure` starts a thread that connects to
/// the host's own socket, speaks the handshake, and then listens.
struct Bench {
    socket: std::path::PathBuf,
    heard: Heard,
    greeted: Arc<AtomicBool>,
    peers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl hl::extension::Supply for Bench {
    fn plan(&self) -> Result<Option<hl::extension::Plan>, String> {
        let manifest = manifest("sample");
        let record = Record {
            name: manifest.name.clone(),
            image_digest: "sha256:aaaa".to_owned(),
            version: manifest.version.clone(),
            granted: manifest.capabilities.clone(),
            enabled: true,
            installed_at: 1,
            pane_providers: manifest.pane_providers.clone(),
        };
        let image = hl::extension::Image {
            reference: "extension:1".to_owned(),
            digest: "sha256:aaaa".to_owned(),
            entrypoint: vec!["/usr/bin/extension".to_owned()],
            user: "1000:1000".to_owned(),
        };
        let spec = hl::extension::SidecarSpec::new(&manifest, &record.granted, &image, &self.socket);
        Ok(Some(hl::extension::Plan {
            record,
            manifest,
            spec,
            workspace: "dev".to_owned(),
        }))
    }

    fn ensure(&self, _plan: &hl::extension::Plan) -> Result<(), String> {
        let socket = self.socket.clone();
        let heard = Arc::clone(&self.heard);
        let greeted = Arc::clone(&self.greeted);
        self.peers
            .lock()
            .expect("peers")
            .push(std::thread::spawn(move || listen(&socket, &heard, &greeted)));
        Ok(())
    }

    fn attend(
        &self,
        _plan: &hl::extension::Plan,
        conversation: &mut hl::extension::Conversation,
    ) -> Result<(), String> {
        // The extension in this suite only listens, so the session ends when it
        // hangs up. Reading is what notices that.
        conversation
            .serve(&ports::services())
            .map_err(|fault| fault.to_string())
    }

    fn halt(&self, _plan: &hl::extension::Plan) {
        for peer in self.peers.lock().expect("peers").drain(..) {
            let _ = peer.join();
        }
    }
}

/// The fake extension: connect, handshake, then write down every interaction
/// the host sends.
fn listen(socket: &Path, heard: &Heard, greeted: &AtomicBool) {
    let Some(stream) = connect(socket) else {
        return;
    };
    let mut wire = Wire::new(stream);
    if shake(&mut wire).is_err() {
        return;
    }
    greeted.store(true, Ordering::Release);
    while let Ok(frame) = wire.receive() {
        let Ok(said) = serde_json::from_slice::<serde_json::Value>(&frame.payload) else {
            continue;
        };
        let Some(id) = said.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        heard.lock().expect("heard").push(id.to_owned());
    }
}

/// Connects to a socket the host may not have bound yet.
fn connect(socket: &Path) -> Option<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Some(stream);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// Reads the welcome and answers it.
fn shake(wire: &mut Wire<UnixStream>) -> Result<(), hl_extension::Transit> {
    let frame = wire.receive()?;
    hl_extension::codec::read_welcome(&frame).map_err(|coding| hl_extension::Transit::Io(coding.to_string()))?;
    let hello = hl_extension::Hello {
        protocol: PROTOCOL,
        name: named("sample"),
        features: Vec::new(),
    };
    wire.send(&hl_extension::codec::hello(&hello).expect("encoded"))
}

fn a_click_on_a_rendered_button_reaches_the_extension() {
    use super::super::extension::{channel, Delivery, Interface, Signal};
    use hl_gui::{Element, EventId, Reconciliation};

    let temporary = tempfile::tempdir().expect("temporary directory");
    let socket = temporary.path().join("run/extension.sock");
    let heard: Heard = Arc::default();
    let greeted = Arc::new(AtomicBool::new(false));
    let host = Rc::new(hl::extension::Host::open(
        Bench {
            socket,
            heard: Arc::clone(&heard),
            greeted: Arc::clone(&greeted),
            peers: Mutex::new(Vec::new()),
        },
        Box::new(|_| ()),
    ));
    assert!(
        until(|| host.standing() == hl::extension::Standing::Duty),
        "the extension connected, got {:?}",
        host.standing()
    );
    // `Duty` is set when the socket is bound, which is earlier than a
    // connection: an interaction handed over before one is accepted is written
    // to nobody and dropped, because the host's writing end is still empty. The
    // host takes that end before it greets, so an extension that has read the
    // welcome is one the host can already speak to -- which is what a click has
    // to wait for, rather than for the standing.
    assert!(
        until(|| greeted.load(Ordering::Acquire)),
        "the extension read the host's welcome"
    );

    let (post, deliveries) = channel();
    let orders = Rc::clone(&host);
    let (widget, mut page) = Interface::new(
        deliveries,
        Rc::new(move |signal: Signal| match signal {
            Signal::Interaction(event) => orders.accept(hl::extension::Order::Interaction(event)),
            Signal::InteractionAt { slot, event } => {
                orders.accept(hl::extension::Order::InteractionAt(hl_extension::SurfaceEvent {
                    slot,
                    event,
                }));
            }
            Signal::Retry => orders.accept(hl::extension::Order::Retry),
        }),
    );
    let described = Element::column().child(Element::button("Restart", EventId::new("restart")).key("restart"));
    let frame = Reconciliation::new().reconcile(&described);
    post.send(Delivery::Frame(frame)).expect("the page is listening");
    page.tick();

    let button = descendants(&widget.clone().upcast())
        .into_iter()
        .find(|found| found.has_css_class("hl-button"))
        .expect("the button reached the page")
        .downcast::<gtk::Button>()
        .expect("a button tag builds a button");
    button.emit_clicked();
    page.tick();

    assert!(
        until(|| heard.lock().expect("heard").iter().any(|id| id == "restart")),
        "the click reached the extension, it heard {:?}",
        heard.lock().expect("heard")
    );
    drop(host);
}

/// In-memory ports, so a conversation can be served with no container runtime
/// and no window.
mod ports {
    use hl_extension::port::{
        ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
        PaneText, TabSummary, TerminalSurface, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
    };
    use hl_extension::{RelativePath, Services, WorkspaceInfo};

    /// The one value every port of this fake host is served from.
    pub(super) struct Ports;
    impl hl_extension::port::VolumeStore for Ports {}
    impl hl_extension::port::NetworkStore for Ports {}

    impl ContainerInventory for Ports {
        fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
            Ok(Vec::new())
        }

        fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
            Err(HostError::Absent(id.to_owned()))
        }
    }

    impl ContainerControl for Ports {
        fn create(&self, _image: &str, name: &str) -> Result<String, HostError> {
            Ok(name.to_owned())
        }

        fn start(&self, _id: &str) -> Result<(), HostError> {
            Ok(())
        }

        fn stop(&self, _id: &str) -> Result<(), HostError> {
            Ok(())
        }

        fn remove(&self, _id: &str) -> Result<(), HostError> {
            Ok(())
        }
    }

    impl ImageStore for Ports {
        fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
            Ok(Vec::new())
        }

        fn pull(&self, reference: &str) -> Result<ImageSummary, HostError> {
            Err(HostError::Absent(reference.to_owned()))
        }
    }

    impl TerminalSurface for Ports {
        fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
            Ok(Vec::new())
        }

        fn open_tab(&self, title: &str) -> Result<String, HostError> {
            Ok(title.to_owned())
        }

        fn split(&self, slot: &str, _division: Division) -> Result<String, HostError> {
            Ok(slot.to_owned())
        }

        fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
            Ok(())
        }

        fn read(&self, slot: &str, _lines: usize) -> Result<PaneText, HostError> {
            Ok(PaneText {
                slot: slot.to_owned(),
                lines: Vec::new(),
                truncated: false,
            })
        }

        fn close(&self, _slot: &str) -> Result<(), HostError> {
            Ok(())
        }

        fn focus(&self, _slot: &str) -> Result<(), HostError> {
            Ok(())
        }

        fn ratio(&self, _slot: &str, _ratio: f64) -> Result<(), HostError> {
            Ok(())
        }

        fn surface(&self, slot: &str, _division: Division) -> Result<String, HostError> {
            Ok(slot.to_owned())
        }
    }

    impl WorkspaceInventory for Ports {
        fn workspaces(&self) -> Result<Vec<WorkspaceState>, HostError> {
            Ok(Vec::new())
        }
    }

    impl hl_extension::port::WorkspaceControl for Ports {}

    impl hl_extension::port::ExtensionStore for Ports {}

    impl WorkspaceFiles for Ports {
        fn list(&self, _path: &RelativePath) -> Result<Vec<Entry>, HostError> {
            Ok(Vec::new())
        }

        fn read(&self, path: &RelativePath) -> Result<Vec<u8>, HostError> {
            Err(HostError::Absent(path.as_str().to_owned()))
        }

        fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
            Ok(())
        }
    }

    /// The services one fake conversation is served against.
    pub(super) fn services() -> Services<'static> {
        static PORTS: Ports = Ports;
        Services {
            workspace: WorkspaceInfo {
                name: "dev".to_owned(),
                architecture: "arm64".to_owned(),
                image: "alpine:3.20".to_owned(),
            },
            workspaces: &PORTS,
            workspace_control: &PORTS,
            extensions: &PORTS,
            containers: &PORTS,
            control: &PORTS,
            images: &PORTS,
            volumes: &PORTS,
            networks: &PORTS,
            terminal: &PORTS,
            files: &PORTS,
        }
    }
}

/// What a socket can do to the panes of a window: read one, restructure the
/// tab, and put an extension's own interface in a pane beside a shell.
///
/// These run against a window built without an application behind it, because
/// every one of them is about the widget tree and the pane registries rather
/// than about a presented window or a running workspace.
mod panes {
    use std::cell::RefCell;
    #[cfg(feature = "mcp-e2e")]
    use std::io::{Read as _, Write as _};
    #[cfg(feature = "mcp-e2e")]
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gtk::prelude::*;
    use hl_extension::port::{Division, HostError, LayoutNode, Occupant, TerminalSurface};
    use hl_extension::ExtensionName;
    use hl_ws_term::session::{PaneNode, SurfacePane};

    use super::super::super::terminal::{
        Adjustment, PaneChooser, PaneChrome, Panes, ProductionPaneLauncher, Reading, Slots, Surface, Tabs, TermWin,
        Window, WindowSession, ABSENCE,
    };
    use super::super::Console;
    use super::super::{Gallery, Shelf, Surfaces};
    use super::Fixture;
    use hl::config::WorkspaceConfig;
    use vte4::prelude::TerminalExt as _;

    /// A window with one tab holding one terminal pane.
    struct Bench {
        window: Rc<TermWin>,
        page: gtk::Box,
    }

    impl Bench {
        fn new() -> Self {
            let workspace = WorkspaceConfig::new("dev", "alpine:3.20", hl_ws::Arch::Arm64);
            let window = Window::bench(&workspace);
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            page.set_hexpand(true);
            page.set_vexpand(true);
            drop(Tabs::new(&window).add("shell 1", None, &page, true));
            Self { window, page }
        }

        /// A terminal pane in the tab, registered under a fresh slot.
        fn shell(&self) -> (vte4::Terminal, String) {
            let terminal = vte4::Terminal::new();
            let slot = Window::slot(&self.window);
            Slots::new(&self.window).hold(&terminal, slot.clone());
            self.page.append(&PaneChrome::wrap(&self.window, &terminal));
            (terminal, slot)
        }

        /// A registered terminal backed by a real raw PTY. The returned slave
        /// is the guest side: socket input arrives there, while its output is
        /// rendered by VTE and becomes readable through the same socket.
        #[cfg(feature = "mcp-e2e")]
        #[allow(unsafe_code)]
        fn shell_with_pty(&self) -> (vte4::Terminal, String, OwnedFd) {
            let mut master = -1;
            let mut slave = -1;
            // SAFETY: openpty initializes both descriptors; ownership is
            // adopted exactly once immediately below.
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &raw mut master,
                        &raw mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                },
                0
            );
            // SAFETY: successful openpty returned two unique live descriptors.
            let master = unsafe { OwnedFd::from_raw_fd(master) };
            let slave = unsafe { OwnedFd::from_raw_fd(slave) };
            let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: the live slave initializes attributes.
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), attributes.as_mut_ptr()) },
                0
            );
            // SAFETY: successful tcgetattr initialized attributes.
            let mut attributes = unsafe { attributes.assume_init() };
            // SAFETY: attributes is initialized and exclusively borrowed.
            unsafe { libc::cfmakeraw(&raw mut attributes) };
            // SAFETY: the slave and attributes remain live for this call.
            assert_eq!(
                unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &raw const attributes) },
                0
            );
            let pty = vte4::Pty::foreign_sync(master, gtk::gio::Cancellable::NONE).expect("foreign PTY");
            let terminal = vte4::Terminal::new();
            terminal.set_pty(Some(&pty));
            let slot = Window::slot(&self.window);
            Slots::new(&self.window).hold(&terminal, slot.clone());
            self.page.append(&PaneChrome::wrap(&self.window, &terminal));
            (terminal, slot, slave)
        }

        /// Another terminal pane, beside an existing one.
        fn beside(&self, pane: &vte4::Terminal) -> (vte4::Terminal, String) {
            let terminal = vte4::Terminal::new();
            let slot = Window::slot(&self.window);
            Slots::new(&self.window).hold(&terminal, slot.clone());
            assert!(
                Panes::divide(
                    &self.window,
                    &Slots::new(&self.window).of(pane).expect("slot"),
                    gtk::Orientation::Horizontal,
                    terminal.upcast_ref()
                ),
                "a pane in a tab can be divided"
            );
            (terminal, slot)
        }

        /// Every slot the window currently holds.
        fn slots(&self) -> Vec<String> {
            Panes::all(&self.window).into_iter().map(|pane| pane.slot).collect()
        }
    }

    fn readable(gallery: &Gallery, extension: &str) {
        let owner = extension.to_owned();
        gallery.enrol_semantics(
            extension,
            Rc::new(move |slot| {
                Ok(hl_extension::PaneSemanticTree {
                    slot: slot.to_owned(),
                    revision: 1,
                    root: hl_extension::SemanticNode {
                        id: 0,
                        role: "surface".to_owned(),
                        label: Some(owner.clone()),
                        value: None,
                        disabled: false,
                        destructive: false,
                        actions: Vec::new(),
                        children: Vec::new(),
                    },
                    truncated: false,
                })
            }),
            Rc::new(|_, _| Ok(())),
        );
    }

    pub(super) fn providers_are_advertised_only_with_a_readable_projection() {
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol(
            "sample",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("dashboard").expect("provider id"),
                title: "Dashboard".to_owned(),
                icon: None,
            }],
            Rc::new(|_| {}),
        );

        assert!(
            gallery.providers().is_empty(),
            "pixels alone are not an inspectable provider"
        );
        assert!(!gallery.offers("sample", "dashboard"));
        let unavailable = gallery
            .semantics("sample", "pane-7")
            .expect("structured unavailable projection");
        assert_eq!(unavailable.slot, "pane-7");
        assert_eq!(unavailable.root.label.as_deref(), Some("Interface unavailable"));
        assert!(unavailable.root.actions.is_empty());

        readable(&gallery, "sample");
        assert!(gallery.offers("sample", "dashboard"));
        assert_eq!(gallery.providers()[0].title, "Dashboard");
        assert_eq!(
            gallery.semantics("sample", "pane-7").unwrap().root.label.as_deref(),
            Some("sample")
        );
    }

    /// Runs the main loop until a condition holds, which is how text fed to a
    /// terminal becomes text the terminal is showing.
    fn until(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        condition()
    }

    #[cfg(feature = "mcp-e2e")]
    pub(super) fn mcp_socket_changes_native_ui() {
        use hl_extension::port::{
            ContainerControl, ContainerInventory, Entry, ExtensionStore, ImageStore, NetworkStore, VolumeStore,
            WorkspaceControl, WorkspaceFiles, WorkspaceInventory,
        };
        use hl_extension::{Authority, Capability, Grant, RelativePath, Services, WorkspaceInfo};
        use std::process::Command;

        struct Unused;
        impl ContainerInventory for Unused {
            fn list(&self) -> Result<Vec<hl_extension::port::ContainerSummary>, HostError> {
                unreachable!()
            }
            fn inspect(&self, _: &str) -> Result<hl_extension::port::ContainerSummary, HostError> {
                unreachable!()
            }
        }
        impl ContainerControl for Unused {
            fn create(&self, _: &str, _: &str) -> Result<String, HostError> {
                unreachable!()
            }
            fn start(&self, _: &str) -> Result<(), HostError> {
                unreachable!()
            }
            fn stop(&self, _: &str) -> Result<(), HostError> {
                unreachable!()
            }
            fn remove(&self, _: &str) -> Result<(), HostError> {
                unreachable!()
            }
        }
        impl ImageStore for Unused {
            fn list(&self) -> Result<Vec<hl_extension::port::ImageSummary>, HostError> {
                unreachable!()
            }
            fn pull(&self, _: &str) -> Result<hl_extension::port::ImageSummary, HostError> {
                unreachable!()
            }
        }
        impl VolumeStore for Unused {}
        impl NetworkStore for Unused {}
        impl WorkspaceInventory for Unused {
            fn workspaces(&self) -> Result<Vec<hl_extension::port::WorkspaceState>, HostError> {
                unreachable!()
            }
        }
        impl WorkspaceControl for Unused {}
        impl ExtensionStore for Unused {}
        impl WorkspaceFiles for Unused {
            fn list(&self, _: &RelativePath) -> Result<Vec<Entry>, HostError> {
                unreachable!()
            }
            fn read(&self, _: &RelativePath) -> Result<Vec<u8>, HostError> {
                unreachable!()
            }
            fn write(&self, _: &RelativePath, _: &[u8]) -> Result<(), HostError> {
                unreachable!()
            }
        }

        let fixture = super::Fixture::new(&[("agent-extension", false)]);
        let view = Rc::clone(&fixture.view);
        view.select_name("Extensions");
        let bench = Bench::new();
        let (_terminal, terminal_slot, slave) = bench.shell_with_pty();
        let mut guest_side = std::fs::File::from(slave);
        guest_side.write_all(b"agent-ready\r\n").expect("seed guest output");
        let guest = std::thread::spawn(move || {
            let expected = b"agent-status\n";
            let mut received = vec![0_u8; expected.len()];
            guest_side.read_exact(&mut received).expect("read MCP terminal input");
            assert_eq!(received, expected, "MCP input reached the guest verbatim");
            let answer = b"agent-received:agent-status\r\n";
            guest_side.write_all(answer).expect("write guest response");
        });
        let gallery = Gallery::new();
        gallery.enrol_native(view.semantic_registry());
        let (post, deliveries) = super::super::super::extension::channel();
        let (widget, reference) = super::super::super::extension::Interface::new(
            deliveries,
            Rc::new(|_: super::super::super::extension::Signal| {}),
        );
        let reference = reference.install();
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.append(&widget);
        gallery.enrol("containers", &widget, &holder, &[], Rc::new(|_| {}));
        let weak = Rc::downgrade(&reference);
        gallery.enrol_panes(
            "containers",
            Rc::new(move |slot| {
                weak.upgrade()
                    .map(|page| page.borrow_mut().pane(slot))
                    .unwrap_or_else(|| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast())
            }),
        );
        let weak = Rc::downgrade(&reference);
        gallery.enrol_semantics(
            "containers",
            Rc::new(move |slot| {
                weak.upgrade()
                    .ok_or_else(|| HostError::Absent("reference extension surface closed".into()))?
                    .borrow()
                    .semantics(slot)
            }),
            Rc::new(|_, _| Err(HostError::Conflict("the reference proof is read-only".into()))),
        );
        Window::exhibit(&bench.window, gallery.clone());
        let surface_slot = Console::surface(&bench.window, Some("containers"), &terminal_slot, Division::Beside)
            .expect("mount reference extension surface beside the terminal");
        let frame = extension::Extension::new()
            .observe(Vec::new())
            .into_iter()
            .find_map(|request| match request {
                hl_extension::Request::InterfaceRender { frame } => Some(frame),
                _ => None,
            })
            .expect("reference extension renders a frame");
        post.send(super::super::super::extension::Delivery::FrameAt {
            slot: surface_slot,
            frame,
        })
        .expect("queue reference extension frame");
        let (relay, errands) = hl::extension::Relay::open();
        let console = Console::new(&bench.window, errands);

        let temporary = tempfile::tempdir().expect("socket directory");
        let socket = temporary.path().join("extension.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind real extension socket");
        let served = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("MCP session connects");
            let authority = Authority::new(
                ExtensionName::new("mcp-e2e").unwrap(),
                Grant::new([
                    Capability::TerminalRead,
                    Capability::TerminalOutput,
                    Capability::TerminalControl,
                    Capability::PaneObserve,
                    Capability::PaneSemanticRead,
                    Capability::PaneSemanticControl,
                ]),
                Vec::new(),
            );
            let unused = Unused;
            let services = Services {
                workspace: WorkspaceInfo {
                    name: "dev".into(),
                    architecture: "arm64".into(),
                    image: "alpine:3.20".into(),
                },
                workspaces: &unused,
                workspace_control: &unused,
                containers: &unused,
                control: &unused,
                images: &unused,
                volumes: &unused,
                networks: &unused,
                terminal: &relay,
                files: &unused,
                extensions: &unused,
            };
            let mut conversation =
                hl::extension::Conversation::new(stream, authority, "dev", hl::extension::Queue::new())
                    .expect("conversation");
            conversation.greet().expect("real handshake");
            conversation.serve(&services).expect("real session");
        });

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root");
        let script = root.join("extensions/mcp/test/native-socket-e2e.mjs");
        assert!(
            root.join("extensions/node_modules/@modelcontextprotocol/sdk").exists(),
            "run `npm ci` in extensions before the explicit mcp-e2e target"
        );
        let mut child = Command::new("node")
            .arg(script)
            .arg(&socket)
            .arg(&terminal_slot)
            .current_dir(root.join("extensions"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn real MCP client/server");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && child.try_wait().expect("poll MCP child").is_none() {
            console.drain();
            gtk::glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = child.wait_with_output().expect("MCP child output");
        assert!(
            output.status.success(),
            "MCP bridge failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("<label>Extensions</label>"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("agent-received:agent-status"));
        assert_eq!(view.shown().as_deref(), Some("Extensions"));
        guest.join().expect("guest PTY responder");
        served.join().expect("conversation thread");
    }

    pub(super) fn native_workspace_semantics_cross_the_terminal_request_bridge() {
        use super::super::super::semantic::{ActionKind, Registry, Value};
        let bench = Bench::new();
        let invoked = Rc::new(std::cell::Cell::new(false));
        let marked = Rc::clone(&invoked);
        let registry = Registry::new("workspace");
        registry.register(
            "workspace/page/Settings",
            "tab",
            Some("Settings"),
            Some(Value::Public("false")),
            &[ActionKind::Invoke],
            Rc::new(move |_, _| marked.set(true)),
        );
        let gallery = Gallery::new();
        gallery.enrol_native(registry);
        Window::exhibit(&bench.window, gallery);
        let (relay, errands) = hl::extension::Relay::open();
        let relay = std::sync::Arc::new(relay);
        let console = Console::new(&bench.window, errands);

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        std::thread::spawn(move || sent.send(request.pane_inventory()).unwrap());
        let inventory = loop {
            console.drain();
            if let Ok(inventory) = received.try_recv() {
                break inventory.expect("native pane discovery crossed the request bridge");
            }
            gtk::glib::MainContext::default().iteration(false);
        };
        assert!(!inventory.truncated);
        assert!(inventory
            .panes
            .iter()
            .any(|pane| { pane.slot == "workspace" && pane.kind == hl_extension::PaneKind::Native }));

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        std::thread::spawn(move || sent.send(request.semantics("workspace")).unwrap());
        let tree = loop {
            console.drain();
            if let Ok(tree) = received.try_recv() {
                break tree.expect("native semantic read crossed the request bridge");
            }
            gtk::glib::MainContext::default().iteration(false);
        };
        assert_eq!(tree.slot, "workspace");
        let settings = tree
            .root
            .children
            .iter()
            .find(|node| node.label.as_deref() == Some("Settings"))
            .unwrap();

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        let action = hl_extension::PaneSemanticAction {
            revision: tree.revision,
            node: settings.id,
            action: hl_extension::SemanticActionKind::Invoke,
            value: None,
        };
        std::thread::spawn(move || sent.send(request.semantic_action("workspace", &action)).unwrap());
        loop {
            console.drain();
            if let Ok(result) = received.try_recv() {
                result.expect("native semantic action crossed the request bridge");
                break;
            }
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(invoked.get(), "the socket-facing action reached its GTK owner");
    }

    /// What a bounded read of one pane answered with.
    fn lines(bench: &Bench, slot: &str, bound: usize) -> Vec<String> {
        match Panes::read(&bench.window, slot, bound) {
            Reading::Text(text) => text.lines,
            other => panic!("a shell pane shows text, got {other:?}"),
        }
    }

    pub(super) fn reading_a_pane_hands_back_what_was_written_to_it() {
        let bench = Bench::new();
        let (terminal, slot) = bench.shell();
        terminal.feed(b"the quick brown fox\r\n");

        assert!(
            until(|| lines(&bench, &slot, 100)
                .iter()
                .any(|line| line.contains("quick brown"))),
            "the pane hands back what was written to it, got {:?}",
            lines(&bench, &slot, 100)
        );
        assert_eq!(
            Panes::read(&bench.window, "no-such-pane", 100),
            Reading::Absent,
            "a slot naming no pane is refused rather than answered with nothing"
        );
    }

    pub(super) fn a_pane_read_never_answers_with_more_than_it_was_allowed() {
        let bench = Bench::new();
        let (terminal, slot) = bench.shell();
        for index in 0..60 {
            terminal.feed(format!("line {index}\r\n").as_bytes());
        }
        assert!(
            until(|| lines(&bench, &slot, 200).iter().any(|line| line.contains("line 59"))),
            "the pane caught up with what was fed to it"
        );

        let Reading::Text(bounded) = Panes::read(&bench.window, &slot, 5) else {
            panic!("a shell pane shows text");
        };

        assert!(
            bounded.lines.len() <= 5,
            "the bound bounds, got {}",
            bounded.lines.len()
        );
        assert!(bounded.truncated, "and says that older lines were left behind");
        assert!(
            bounded.lines.last().is_some_and(|line| line.contains("line 59")),
            "the tail is what is kept, got {:?}",
            bounded.lines
        );
    }

    pub(super) fn dividing_a_pane_produces_a_slot_that_can_be_addressed() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let before = bench.slots();

        let (_second, two) = bench.beside(&first);

        assert!(!before.contains(&two), "the slot is new");
        assert_eq!(bench.slots().len(), before.len() + 1, "and there is one more pane");
        assert!(Panes::at(&bench.window, &two).is_some(), "addressable by its own slot");
        assert_eq!(Panes::ratio(&bench.window, &two, 0.25), Adjustment::Set);
        assert_eq!(
            Panes::ratio(&bench.window, "no-such-pane", 0.25),
            Adjustment::Absent,
            "a ratio for a pane that is not there is refused"
        );
        assert!(Panes::focus(&bench.window, &one), "focus moves by slot");
    }

    pub(super) fn closing_a_pane_by_slot_removes_that_one_and_leaves_the_rest() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let (_second, two) = bench.beside(&first);

        assert!(Panes::close(&bench.window, &two), "the pane was closed");

        assert_eq!(bench.slots(), vec![one], "exactly the named pane is gone");
        assert!(!Panes::close(&bench.window, &two), "and closing it again finds nothing");
    }

    pub(super) fn a_pane_can_hold_an_extensions_interface_beside_a_shell() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        assert_ne!(
            Slots::new(&bench.window).adopt(Some(&one)),
            one,
            "duplicated persisted slots cannot alias an already live pane"
        );
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        interface.add_css_class(super::SURFACE);
        home.append(&interface);
        gallery.enrol("sample", &interface, &home, &[], Rc::new(|_| {}));
        Window::exhibit(&bench.window, gallery.clone());
        let slot = Window::slot(&bench.window);
        let pane = Surface::build(&bench.window, "sample", None, slot.clone());
        assert!(Panes::divide(&bench.window, &one, gtk::Orientation::Horizontal, &pane));

        let held = Panes::at(&bench.window, &slot).expect("the surface pane is addressable");
        assert_eq!(held.occupant, Occupant::Surface, "and says what is in it");
        assert!(
            super::descendants(&pane)
                .iter()
                .any(|found| found == interface.upcast_ref::<gtk::Widget>()),
            "the extension's own interface is what the pane holds"
        );
        assert!(
            Panes::all(&bench.window).iter().any(|pane| pane.slot == one),
            "the shell beside it is still a pane"
        );
        assert!(
            matches!(Panes::read(&bench.window, &slot, 10), Reading::Drawn),
            "and it is not pretending to be a shell"
        );

        let (relay, errands) = hl::extension::Relay::open();
        let relay = std::sync::Arc::new(relay);
        let console = Console::new(&bench.window, errands);
        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        std::thread::spawn(move || sent.send(request.pane_inventory()).unwrap());
        let inventory = loop {
            console.drain();
            if let Ok(inventory) = received.try_recv() {
                break inventory.expect("pane inventory");
            }
            gtk::glib::MainContext::default().iteration(false);
        };
        assert!(inventory.panes.iter().any(|pane| pane.slot == slot));

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        let surface_slot = slot.clone();
        std::thread::spawn(move || sent.send(request.semantics(&surface_slot)).unwrap());
        let projection = loop {
            console.drain();
            if let Ok(projection) = received.try_recv() {
                break projection.expect("listed surface remains readable through the pane port");
            }
            gtk::glib::MainContext::default().iteration(false);
        };
        assert_eq!(projection.slot, slot);
        assert_eq!(projection.root.label.as_deref(), Some("Interface unavailable"));
        assert!(projection.root.disabled);
        assert!(projection.root.actions.is_empty());

        assert!(
            Panes::close(&bench.window, &slot),
            "the surface pane closes like any other"
        );
        assert_eq!(
            interface.parent().as_ref(),
            Some(home.upcast_ref::<gtk::Widget>()),
            "and hands the interface back to its page rather than taking it away"
        );
        let replacement = Window::slot(&bench.window);
        assert_ne!(replacement, slot, "a replacement at the same UI position gets a fresh authority identity");

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        let stale = slot.clone();
        std::thread::spawn(move || sent.send(request.write(&stale, b"stale")).unwrap());
        loop {
            console.drain();
            if let Ok(answer) = received.try_recv() {
                assert!(matches!(answer, Err(HostError::Absent(_))));
                break;
            }
            gtk::glib::MainContext::default().iteration(false);
        }

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        let stale = slot.clone();
        std::thread::spawn(move || {
            sent.send(request.resize_grid(&stale, hl_extension::port::GridSize { columns: 80, rows: 24 }))
                .unwrap()
        });
        loop {
            console.drain();
            if let Ok(answer) = received.try_recv() {
                assert!(matches!(answer, Err(HostError::Absent(_))));
                break;
            }
            gtk::glib::MainContext::default().iteration(false);
        }

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        let stale = slot.clone();
        std::thread::spawn(move || sent.send(request.semantics(&stale)).unwrap());
        loop {
            console.drain();
            if let Ok(answer) = received.try_recv() {
                assert!(matches!(answer, Err(HostError::Absent(_))));
                break;
            }
            gtk::glib::MainContext::default().iteration(false);
        }

        let (sent, received) = std::sync::mpsc::channel();
        let request = std::sync::Arc::clone(&relay);
        let stale = slot.clone();
        std::thread::spawn(move || {
            sent.send(request.semantic_action(
                &stale,
                &hl_extension::PaneSemanticAction {
                    revision: 0,
                    node: 0,
                    action: hl_extension::SemanticActionKind::Invoke,
                    value: None,
                },
            ))
            .unwrap()
        });
        loop {
            console.drain();
            if let Ok(answer) = received.try_recv() {
                assert!(matches!(answer, Err(HostError::Absent(_))));
                break;
            }
            gtk::glib::MainContext::default().iteration(false);
        }
        drop(first);
    }

    pub(super) fn a_pane_chooser_switches_to_a_provider_and_back_to_its_shell() {
        let bench = Bench::new();
        let (terminal, slot) = bench.shell();
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        let selected = Rc::new(RefCell::new(None));
        let selection = Rc::clone(&selected);
        gallery.enrol(
            "postgres",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("database").expect("provider id"),
                title: "Postgres".to_owned(),
                icon: None,
            }],
            Rc::new(move |provider| *selection.borrow_mut() = Some(provider)),
        );
        readable(&gallery, "postgres");
        Window::exhibit(&bench.window, gallery.clone());
        let chrome = Panes::at(&bench.window, &slot).expect("pane chrome").widget;

        assert_eq!(gallery.providers()[0].title, "Postgres");
        PaneChooser::provider(&bench.window, "postgres", "database");
        assert_eq!(
            Panes::at(&bench.window, &slot).expect("switched pane").occupant,
            Occupant::Surface
        );
        assert!(
            gallery.holds("postgres"),
            "the overview remains registered while its interface is borrowed"
        );
        assert_eq!(
            selected
                .borrow()
                .as_ref()
                .map(|selection| selection.pane_provider.as_str()),
            Some("database"),
            "the extension is told which named view it should render"
        );
        assert_eq!(
            selected.borrow().as_ref().map(|selection| selection.slot.as_str()),
            Some(slot.as_str()),
            "the selection identifies this mount rather than a global provider"
        );
        let topology = Console::topology(&bench.window).expect("provider topology");
        let LayoutNode::Pane { pane, .. } = &topology.tabs[0].root else {
            panic!("the unsplit provider is one pane")
        };
        let identity = pane.provider.as_ref().expect("surface provider identity");
        assert_eq!(identity.extension, "postgres");
        assert_eq!(identity.provider, "database");

        let chooser = super::descendants(&chrome)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
            .expect("pane chooser");
        PaneChooser::populate(&bench.window, &chooser);
        assert_eq!(
            chooser.tooltip_text().as_deref(),
            Some("Choose pane content; currently showing Postgres · postgres")
        );
        let popover = chooser.popover().expect("chooser popover");
        let widgets = super::descendants(popover.upcast_ref::<gtk::Widget>());
        assert!(widgets.iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text() == "Currently showing Postgres · postgres")
        }));
        assert!(widgets.iter().any(|widget| {
            widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                button.label().as_deref() == Some("Postgres") && button.has_css_class("suggested-action")
            })
        }));
        assert!(
            widgets.iter().any(|widget| {
                widget
                    .downcast_ref::<gtk::Box>()
                    .is_some_and(|choices| choices.width_request() == 200)
            }),
            "the popover has a compact minimum rather than forcing a wide pane"
        );

        PaneChooser::terminal(&bench.window);
        let restored = Panes::at(&bench.window, &slot).expect("restored pane");
        assert_eq!(restored.occupant, Occupant::Terminal);
        assert_eq!(
            restored.widget, chrome,
            "the pane keeps one stable chrome across occupants"
        );
        assert_eq!(restored.content, terminal.upcast::<gtk::Widget>());
        assert_eq!(interface.parent().as_ref(), Some(home.upcast_ref::<gtk::Widget>()));
    }

    pub(super) fn an_existing_pane_chooser_discovers_a_later_provider() {
        let bench = Bench::new();
        let chooser = PaneChooser::button(&bench.window);
        assert_eq!(chooser.icon_name().as_deref(), Some("view-grid-symbolic"));
        assert_eq!(
            chooser.tooltip_text().as_deref(),
            Some("Choose pane content; currently showing Terminal")
        );
        let labels = || {
            chooser
                .popover()
                .into_iter()
                .flat_map(|popover| super::descendants(popover.upcast_ref::<gtk::Widget>()))
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .filter_map(|button| button.label())
                .map(|label| label.to_string())
                .collect::<Vec<String>>()
        };
        assert_eq!(labels(), ["Terminal"], "the chooser exists before providers do");
        let empty_copy: Vec<_> = chooser
            .popover()
            .into_iter()
            .flat_map(|popover| super::descendants(popover.upcast_ref::<gtk::Widget>()))
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .map(|label| label.text().to_string())
            .collect();
        assert!(empty_copy.iter().any(|label| label == "No extension views available"));
        assert!(empty_copy.iter().any(|label| label.contains("Install or enable")));

        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol(
            "postgres",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("database").expect("provider id"),
                title: "Postgres".to_owned(),
                icon: None,
            }],
            Rc::new(|_| {}),
        );
        readable(&gallery, "postgres");
        Window::exhibit(&bench.window, gallery);
        PaneChooser::populate(&bench.window, &chooser);
        assert_eq!(
            labels(),
            ["Terminal", "Postgres"],
            "an old tab reads the live catalogue"
        );
    }

    pub(super) fn each_split_chooser_switches_its_own_pane_without_stealing_terminal_focus() {
        let bench = Bench::new();
        let (first, first_slot) = bench.shell();
        let (second, second_slot) = bench.beside(&first);
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol(
            "postgres",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("database").expect("provider id"),
                title: "Postgres".to_owned(),
                icon: None,
            }],
            Rc::new(|_| {}),
        );
        readable(&gallery, "postgres");
        Window::exhibit(&bench.window, gallery);
        assert!(Panes::focus(&bench.window, &first_slot));
        assert!(until(|| first.has_focus()), "the first terminal owns keyboard focus");

        let second_pane = Panes::at(&bench.window, &second_slot).expect("second pane");
        let chooser = super::descendants(&second_pane.widget)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
            .expect("every split leaf owns a chooser");
        assert!(chooser.is_focusable(), "the chooser is keyboard reachable");
        PaneChooser::populate(&bench.window, &chooser);
        let postgres = chooser
            .popover()
            .into_iter()
            .flat_map(|popover| super::descendants(popover.upcast_ref::<gtk::Widget>()))
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Postgres"))
            .expect("provider choice");
        postgres.emit_clicked();

        assert_eq!(
            Panes::at(&bench.window, &first_slot)
                .expect("focused first pane")
                .content,
            first.clone().upcast::<gtk::Widget>(),
            "the globally focused pane is not replaced"
        );
        assert_eq!(
            Panes::at(&bench.window, &second_slot)
                .expect("chosen second pane")
                .occupant,
            Occupant::Surface,
            "the chooser replaces its own leaf"
        );
        assert!(
            first.has_focus(),
            "switching an adjacent pane preserves terminal keyboard focus"
        );

        PaneChooser::populate(&bench.window, &chooser);
        let terminal = chooser
            .popover()
            .into_iter()
            .flat_map(|popover| super::descendants(popover.upcast_ref::<gtk::Widget>()))
            .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
            .find(|button| button.label().as_deref() == Some("Terminal"))
            .expect("terminal is always available");
        terminal.emit_clicked();
        assert_eq!(
            Panes::at(&bench.window, &second_slot)
                .expect("restored second pane")
                .content,
            second.upcast::<gtk::Widget>(),
            "the displaced terminal identity is restored"
        );
    }

    pub(super) fn pane_chooser_groups_and_filters_many_extension_views() {
        let bench = Bench::new();
        let gallery = Gallery::new();
        let mut homes = Vec::new();
        for (extension, providers) in [
            (
                "database-tools",
                [("postgres", "Postgres"), ("mysql", "MySQL"), ("redis", "Redis")],
            ),
            (
                "workspace-tools",
                [
                    ("containers", "Containers"),
                    ("images", "Images"),
                    ("networks", "Networks"),
                ],
            ),
        ] {
            let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
            home.append(&interface);
            let providers: Vec<_> = providers
                .into_iter()
                .map(|(id, title)| hl_extension::PaneProvider {
                    id: ExtensionName::new(id).expect("provider id"),
                    title: title.to_owned(),
                    icon: None,
                })
                .collect();
            gallery.enrol(extension, &interface, &home, &providers, Rc::new(|_| {}));
            readable(&gallery, extension);
            homes.push(home);
        }
        Window::exhibit(&bench.window, gallery);
        let chooser = PaneChooser::button(&bench.window);
        let popover = chooser.popover().expect("chooser popover");
        let widgets = super::descendants(popover.upcast_ref::<gtk::Widget>());
        let labels: Vec<_> = widgets
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
            .map(|label| label.text().to_string())
            .collect();
        assert!(labels.contains(&"database-tools".to_owned()));
        assert!(labels.contains(&"workspace-tools".to_owned()));
        let search = widgets
            .iter()
            .find_map(|widget| widget.downcast_ref::<gtk::SearchEntry>())
            .expect("six providers expose search");
        search.set_text("image");
        assert!(until(|| {
            widgets.iter().any(|widget| {
                widget.downcast_ref::<gtk::Button>().is_some_and(|button| {
                    button.label().as_deref() == Some("Postgres") && !button.property::<bool>("visible")
                })
            })
        }));
        let visible: Vec<_> = widgets
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
            .filter(|button| button.property::<bool>("visible"))
            .filter_map(gtk::Button::label)
            .map(|label| label.to_string())
            .collect();
        assert_eq!(visible, ["Terminal", "Images"]);
        let visible_groups: Vec<_> = widgets
            .iter()
            .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
            .filter(|label| label.has_css_class("caption") && label.property::<bool>("visible"))
            .map(|label| label.text().to_string())
            .collect();
        assert_eq!(visible_groups, ["workspace-tools"]);
        drop(homes);
    }

    fn lifecycle_withdrawal(remove: bool) {
        let fixture = Fixture::new(&[("postgres", true)]);
        let bench = Bench::new();
        let (first, first_slot) = bench.shell();
        let (_second, second_slot) = bench.beside(&first);
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol(
            "postgres",
            &interface,
            &home,
            &[hl_extension::PaneProvider {
                id: ExtensionName::new("database").expect("provider id"),
                title: "Postgres".to_owned(),
                icon: None,
            }],
            Rc::new(|_| {}),
        );
        readable(&gallery, "postgres");
        Window::exhibit(&bench.window, gallery.clone());
        assert!(Panes::focus(&bench.window, &first_slot));
        PaneChooser::provider(&bench.window, "postgres", "database");
        assert_eq!(
            Panes::at(&bench.window, &first_slot).expect("provider pane").occupant,
            Occupant::Surface
        );
        assert!(
            Panes::focus(&bench.window, &second_slot),
            "a different split leaf is selected"
        );

        let window = Rc::downgrade(&bench.window);
        let withdrawn = gallery.clone();
        let withdraw = Rc::new(move |name: &ExtensionName| {
            if let Some(window) = window.upgrade() {
                PaneChooser::withdraw(&window, name.as_str());
            }
            withdrawn.withdraw(name.as_str());
        });
        let surfaces: Surfaces = Rc::new(|_| gtk::Box::new(gtk::Orientation::Vertical, 0).upcast());
        let shelf = Shelf::with_lifecycle(&fixture.view, &fixture.roster, surfaces, Rc::new(|_| {}), withdraw);
        let name = ExtensionName::new("postgres").expect("extension name");
        if remove {
            fixture.roster.borrow_mut().remove(&name).expect("removed");
        } else {
            fixture.roster.borrow_mut().disable(&name).expect("disabled");
        }
        shelf.refresh(&name);

        let frozen = Panes::at(&bench.window, &first_slot).expect("tombstoned pane identity");
        assert_eq!(frozen.occupant, Occupant::Surface);
        assert!(
            super::descendants(&frozen.content)
                .iter()
                .any(|widget| widget.has_css_class(ABSENCE)),
            "withdrawal leaves an explicit recovery placeholder"
        );
        assert_eq!(
            Panes::at(&bench.window, &second_slot).expect("unrelated pane").slot,
            second_slot,
            "the selected sibling keeps its identity"
        );
        assert!(
            gallery.providers().is_empty(),
            "withdrawal disappears from every chooser immediately"
        );
        assert_eq!(interface.parent().as_ref(), Some(home.upcast_ref::<gtk::Widget>()));
        assert_eq!(
            Slots::new(&bench.window).surface(&frozen.content),
            Some((first_slot.clone(), "postgres".to_owned(), Some("database".to_owned()))),
            "the persisted provider identity survives lifecycle withdrawal"
        );

        if !remove {
            gallery.enrol(
                "postgres",
                &interface,
                &home,
                &[hl_extension::PaneProvider {
                    id: ExtensionName::new("database").expect("provider id"),
                    title: "Postgres".to_owned(),
                    icon: None,
                }],
                Rc::new(|_| {}),
            );
            readable(&gallery, "postgres");
            PaneChooser::recover(&bench.window, "postgres");
            assert_eq!(
                interface.parent().as_ref(),
                Some(frozen.content.upcast_ref::<gtk::Widget>())
            );
            PaneChooser::withdraw(&bench.window, "postgres");
            gallery.withdraw("postgres");
        }

        assert!(Panes::focus(&bench.window, &first_slot));
        PaneChooser::terminal(&bench.window);
        let restored = Panes::at(&bench.window, &first_slot).expect("explicitly restored shell");
        assert_eq!(restored.occupant, Occupant::Terminal);
        assert_eq!(restored.content, first.upcast::<gtk::Widget>());
    }

    pub(super) fn disabling_an_extension_tombstones_and_recovers_its_surface_pane() {
        lifecycle_withdrawal(false);
    }

    pub(super) fn removing_an_extension_tombstones_without_displacing_its_shell() {
        lifecycle_withdrawal(true);
    }

    pub(super) fn every_split_leaf_owns_its_chooser_and_topology_is_nested() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let (_second, two) = bench.beside(&first);
        for slot in [&one, &two] {
            let pane = Panes::at(&bench.window, slot).expect("split leaf");
            assert!(PaneChrome::is(&pane.widget), "{slot} has stable pane chrome");
            assert!(
                super::descendants(&pane.widget)
                    .iter()
                    .any(|widget| widget.is::<gtk::MenuButton>()),
                "{slot} owns its chooser"
            );
        }

        let topology = Console::topology(&bench.window).expect("topology");
        assert_eq!(topology.active_tab.as_deref(), Some("p0"));
        assert_eq!(topology.tabs.len(), 1);
        let LayoutNode::Split {
            division,
            first,
            second,
            ..
        } = &topology.tabs[0].root
        else {
            panic!("two leaves are reported as one nested split")
        };
        assert_eq!(*division, Division::Beside);
        let slots = [first.as_ref(), second.as_ref()].map(|node| match node {
            LayoutNode::Pane { pane, .. } => pane.slot.as_str(),
            LayoutNode::Split { .. } => panic!("a leaf became another split"),
        });
        assert_eq!(slots, [one.as_str(), two.as_str()]);
    }

    pub(super) fn two_same_extension_panes_render_independently_by_slot() {
        use super::super::super::extension::{channel, Delivery, Interface};
        use hl_gui::{Element, Reconciliation};

        let bench = Bench::new();
        let (first, one) = bench.shell();
        let (_second, two) = bench.beside(&first);
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let (post, deliveries) = channel();
        let (interface, page) = Interface::new(deliveries, Rc::new(|_| {}));
        home.append(&interface);
        let page = Rc::new(RefCell::new(page));
        gallery.enrol("sample", &interface, &home, &[], Rc::new(|_| {}));
        let retained = Rc::clone(&page);
        gallery.enrol_panes("sample", Rc::new(move |slot| retained.borrow_mut().pane(slot)));
        Window::exhibit(&bench.window, gallery);

        let left = Console::surface(&bench.window, Some("sample"), &one, Division::Below).expect("first surface");
        let right = Console::surface(&bench.window, Some("sample"), &two, Division::Below).expect("second surface");
        assert_ne!(left, right);
        post.send(Delivery::FrameAt {
            slot: left.clone(),
            frame: Reconciliation::new().reconcile(&Element::text("left only")),
        })
        .expect("left frame");
        post.send(Delivery::FrameAt {
            slot: right.clone(),
            frame: Reconciliation::new().reconcile(&Element::text("right only")),
        })
        .expect("right frame");
        assert_eq!(page.borrow_mut().tick(), 2);

        let labels = |slot: &str| {
            let pane = Panes::at(&bench.window, slot).expect("addressed surface remains mounted");
            super::descendants(&pane.widget)
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
                .map(|label| label.text().to_string())
                .collect::<Vec<_>>()
        };
        assert!(labels(&left).iter().any(|label| label == "left only"));
        assert!(!labels(&left).iter().any(|label| label == "right only"));
        assert!(labels(&right).iter().any(|label| label == "right only"));
        assert!(!labels(&right).iter().any(|label| label == "left only"));
        assert!(
            interface.parent().is_some(),
            "the extension overview remains independently available"
        );
    }

    pub(super) fn a_failed_interface_split_leaves_its_surface_where_it_was() {
        let bench = Bench::new();
        let (first, one) = bench.shell();
        let gallery = Gallery::new();
        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol("sample", &interface, &home, &[], Rc::new(|_| {}));
        Window::exhibit(&bench.window, gallery);
        let old = Console::surface(&bench.window, Some("sample"), &one, Division::Beside).expect("the first surface");
        let before = interface.parent();
        // A registered pane under a Grid is addressable, but PaneSplit cannot
        // restructure that parent: terminal layouts accept only their Box and
        // Paned shapes. This reaches the post-borrow rollback path rather than
        // the missing-slot preflight.
        let unsupported = vte4::Terminal::new();
        let unsupported_slot = Window::slot(&bench.window);
        Slots::new(&bench.window).hold(&unsupported, unsupported_slot.clone());
        let grid = gtk::Grid::new();
        grid.attach(&PaneChrome::wrap(&bench.window, &unsupported), 0, 0, 1, 1);
        bench.page.append(&grid);

        let failure = Console::surface(&bench.window, Some("sample"), &unsupported_slot, Division::Below);

        assert!(matches!(failure, Err(HostError::Absent(_))));
        assert!(
            Panes::at(&bench.window, &old).is_some(),
            "the old slot remains addressable"
        );
        assert_eq!(interface.parent(), before, "the same holder still owns the interface");
        assert_eq!(
            Panes::all(&bench.window)
                .iter()
                .filter(|pane| pane.occupant == Occupant::Surface)
                .count(),
            1,
            "failure creates no placeholder surface"
        );
        drop(first);
    }

    pub(super) fn a_restored_surface_without_its_extension_is_frozen_rather_than_a_shell() {
        let bench = Bench::new();
        let gallery = Gallery::new();
        Window::exhibit(&bench.window, gallery.clone());
        let storage = tempfile::tempdir().expect("temporary directory");
        let node = PaneNode::Surface(SurfacePane {
            extension: "departed".to_owned(),
            provider: None,
            slot: Some("7".to_owned()),
        });

        let mut pids = Vec::new();
        let (widget, terminal) = WindowSession::new(&bench.window).build_pane_widget(
            &node,
            storage.path(),
            &mut pids,
            &ProductionPaneLauncher,
        );
        bench.page.append(&widget);

        assert!(terminal.is_none(), "an absent extension is never replaced by a shell");
        assert!(
            super::descendants(&widget)
                .iter()
                .any(|found| found.has_css_class(ABSENCE)),
            "the pane says whose interface belongs in it and that nobody is drawing"
        );
        let held = Panes::at(&bench.window, "7").expect("the restored pane keeps its slot");
        assert_eq!(held.occupant, Occupant::Surface);

        let home = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let interface = gtk::Box::new(gtk::Orientation::Vertical, 0);
        home.append(&interface);
        gallery.enrol("departed", &interface, &home, &[], Rc::new(|_| {}));
        PaneChooser::recover(&bench.window, "departed");
        assert_eq!(
            interface.parent().as_ref(),
            Some(held.content.upcast_ref::<gtk::Widget>()),
            "re-enabling after restart rehydrates the preserved pane"
        );
    }
}
