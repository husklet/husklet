//! What the extension page promises: queued work reaches the widgets under a
//! per-tick cap, nothing is dropped, a stopped extension keeps its last
//! interface on screen, and interaction reaches the caller's sink.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use hl_gui::{Element, Event, EventId, Reconciliation, Tag};

use super::{DRAIN, Delivery, Interface, Post, Signal, channel};

/// Everything the sink was handed, in order.
type Record = Rc<RefCell<Vec<Signal>>>;

/// One page, its queue, and what its sink recorded.
struct Fixture {
    widget: gtk::Box,
    page: Interface,
    post: Post,
    recorded: Record,
    reconciliation: Reconciliation,
}

impl Fixture {
    fn new() -> Self {
        let (post, deliveries) = channel();
        let recorded: Record = Rc::new(RefCell::new(Vec::new()));
        let sink = recorded.clone();
        let (widget, page) = Interface::new(deliveries, Rc::new(move |signal| sink.borrow_mut().push(signal)));
        Self {
            widget,
            page,
            post,
            recorded,
            reconciliation: Reconciliation::new(),
        }
    }

    /// Queues the frame that turns the current description into `described`.
    fn describe(&mut self, described: &Element) {
        let frame = self.reconciliation.reconcile(described);
        self.post.send(Delivery::Frame(frame)).expect("the page is listening");
    }

    /// Every widget on the page, parents before children.
    fn widgets(&self) -> Vec<gtk::Widget> {
        let mut found = vec![self.widget.clone().upcast::<gtk::Widget>()];
        let mut index = 0;
        while index < found.len() {
            found.extend(offspring(&found[index]));
            index += 1;
        }
        found
    }

    /// The first widget carrying a tag's style class, which is the adapter's
    /// own public naming.
    fn tagged(&self, tag: Tag) -> Option<gtk::Widget> {
        let class = format!("hl-{}", tag.as_str().to_ascii_lowercase());
        self.widgets().into_iter().find(|widget| widget.has_css_class(&class))
    }
}

fn offspring(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut children = Vec::new();
    let mut cursor = widget.first_child();
    while let Some(child) = cursor {
        cursor = child.next_sibling();
        children.push(child);
    }
    children
}

/// A small interface with one button, keyed so a redescription is a no-op.
fn panel(title: &str) -> Element {
    Element::column()
        .child(Element::heading(title).key("title"))
        .child(Element::button("Restart", EventId::new("restart")).key("restart"))
}

fn provider_authority_waits_for_a_valid_frame() {
    let (post, deliveries) = channel();
    let ready = Rc::new(Cell::new(0));
    let observed = Rc::clone(&ready);
    let (_widget, mut page) = Interface::with_lifecycle(
        deliveries,
        Rc::new(|_| {}),
        Rc::new(|_| {}),
        Rc::new(move || observed.set(observed.get() + 1)),
    );

    let mut out_of_sequence = Reconciliation::new();
    let _ = out_of_sequence.reconcile(&Element::heading("discarded initial frame"));
    let rejected = out_of_sequence.reconcile(&Element::heading("update without root"));
    post.send(Delivery::Frame(rejected)).expect("page listening");
    page.tick();
    assert_eq!(ready.get(), 0, "a rejected frame cannot publish provider authority");

    let accepted = Reconciliation::new().reconcile(&Element::heading("ready"));
    post.send(Delivery::Frame(accepted)).expect("page listening");
    page.tick();
    assert_eq!(ready.get(), 1, "the first valid frame publishes provider authority");
}

/// Every scenario runs inside one test, on the binary's toolkit thread.
///
/// GTK belongs to whichever thread entered it and libtest gives every `#[test]`
/// its own, so the scenarios are handed to `test_support`, which owns the one
/// thread in this process that entered GTK. This test and the extension-shelf
/// test both need that thread, and entering GTK in their own is what used to
/// leave whichever ran second either panicking or skipped.
#[test]
fn an_extension_page_renders_what_is_queued_and_survives_the_extension() {
    let ran = crate::test_support::on_the_toolkit_thread(|| {
        provider_authority_waits_for_a_valid_frame();
        a_queued_frame_puts_widgets_on_the_page();
        an_identical_frame_changes_nothing();
        a_burst_beyond_the_tick_bound_stays_queued();
        a_stopped_extension_keeps_its_widgets_and_says_so();
        a_structured_fault_reaches_lifecycle_on_the_toolkit_tick();
        a_rendered_button_reaches_the_sink();
        retained_pane_actions_keep_their_slot();
        retiring_a_pane_discards_its_queued_interaction();
        retired_panes_ignore_late_frames_until_explicitly_remounted();
        oversized_tree_growth_is_atomic_isolated_and_remountable();
        semantics_are_redacted_and_actions_reject_stale_revisions();
        command_palette_exposes_typed_semantic_actions();
        tag_input_exposes_value_actions_and_authored_tags();
        validation_summary_is_readable_and_keeps_corrective_actions();
        diff_lines_project_status_and_bounded_text();
        markdown_preserves_bounded_source_in_semantics();
        json_preserves_source_in_semantics();
        stack_frames_project_function_and_location();
        hex_view_projects_binary_text_into_semantics();
        sparkline_projects_bounded_samples_into_semantics();
        file_browser_keeps_its_semantic_role();
        flame_graph_projects_profile_frames_into_semantics();
        memory_map_projects_exact_regions_into_semantics();
        semantic_actions_are_safe_by_default_and_preserve_authored_danger();
        disabled_and_hidden_controls_are_not_advertised_as_actions();
    });
    if !ran {
        eprintln!("skipped: no display connection, so the extension page cannot be rendered");
    }
}

fn memory_map_projects_exact_regions_into_semantics() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::memory_map([
        hl_gui::MemoryRegion::new(0x1000, 0x2000, "r-xp", "/bin/demo").expect("region"),
        hl_gui::MemoryRegion::new(0x3000, 0x5000, "rw-p", "[heap]").expect("region"),
    ]));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let map = &tree.root.children[0];
    assert_eq!(map.role, "MemoryMap");
    assert_eq!(
        map.value.as_deref(),
        Some(
            "0000000000001000-0000000000002000\tr-xp\t4096\t/bin/demo\n0000000000003000-0000000000005000\trw-p\t8192\t[heap]"
        )
    );
    assert!(map.actions.is_empty());
}

fn file_browser_keeps_its_semantic_role() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::file_browser());
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let browser = &tree.root.children[0];
    assert_eq!(browser.role, "FileBrowser");
    assert!(browser.actions.is_empty());
}

fn flame_graph_projects_profile_frames_into_semantics() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::flame_graph([
        hl_gui::FlameFrame::new("compiler::parse", 91).expect("frame"),
        hl_gui::FlameFrame::new("compiler::emit", 34).expect("frame"),
    ]));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let profile = &tree.root.children[0];
    assert_eq!(profile.role, "FlameGraph");
    assert_eq!(
        profile.value.as_deref(),
        Some("91\tcompiler::parse\n34\tcompiler::emit")
    );
    assert!(profile.actions.is_empty());
}

fn json_preserves_source_in_semantics() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::json_view(r#"{"nested":{"ready":true}}"#));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let document = &tree.root.children[0];
    assert_eq!(document.role, "JsonView");
    assert_eq!(document.value.as_deref(), Some(r#"{"nested":{"ready":true}}"#));
    assert!(document.actions.is_empty());
}

fn stack_frames_project_function_and_location() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::stack_trace().child(Element::stack_frame("host::dispatch", "src/host.rs:42")));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let frame = &tree.root.children[0].children[0];
    assert_eq!(frame.role, "StackFrame");
    assert_eq!(frame.label.as_deref(), Some("host::dispatch"));
    assert_eq!(frame.value.as_deref(), Some("src/host.rs:42"));
}

fn sparkline_projects_bounded_samples_into_semantics() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::sparkline([1.0, 3.0, 2.0]));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let trend = &tree.root.children[0];
    assert_eq!(trend.role, "Sparkline");
    assert_eq!(trend.value.as_deref(), Some("1,3,2"));
    assert!(trend.actions.is_empty());
}

fn hex_view_projects_binary_text_into_semantics() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::hex_view(hl_gui::HexSource::Exact(b"\x7fELF")));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let binary = &tree.root.children[0];
    assert_eq!(binary.role, "HexView");
    assert_eq!(
        binary.value.as_deref(),
        Some("00000000  7f 45 4c 46                                       |.ELF|\n")
    );
    assert!(binary.actions.is_empty());
}

fn diff_lines_project_status_and_bounded_text() {
    let mut fixture = Fixture::new();
    let described = Element::diff_viewer()
        .child(Element::diff_line("-", "image: app:v1"))
        .child(Element::diff_line("+", "image: app:v2"));
    fixture.describe(&described);
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let diff = &tree.root.children[0];
    assert_eq!(diff.role, "DiffViewer");
    assert_eq!(diff.children.len(), 2);
    assert_eq!(diff.children[0].label.as_deref(), Some("-"));
    assert_eq!(diff.children[0].value.as_deref(), Some("image: app:v1"));
    assert_eq!(diff.children[1].label.as_deref(), Some("+"));
    assert_eq!(diff.children[1].value.as_deref(), Some("image: app:v2"));
}

fn validation_summary_is_readable_and_keeps_corrective_actions() {
    let mut fixture = Fixture::new();
    let described = Element::validation_summary("2 problems found")
        .detail("Correct the highlighted fields")
        .child(Element::button("Review name", EventId::new("review-name")));
    fixture.describe(&described);
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let summary = &tree.root.children[0];
    assert_eq!(summary.role, "ValidationSummary");
    assert_eq!(summary.label.as_deref(), Some("2 problems found"));
    assert_eq!(summary.value.as_deref(), Some("Correct the highlighted fields"));
    assert_eq!(summary.children[0].label.as_deref(), Some("Review name"));
    assert_eq!(
        summary.children[0].actions,
        vec![hl_extension::SemanticActionKind::Invoke]
    );
}

fn tag_input_exposes_value_actions_and_authored_tags() {
    let mut fixture = Fixture::new();
    let described = Element::tag_input(EventId::new("tag-change"), EventId::new("tag-submit"))
        .value("new")
        .child(Element::toggle_button("backend", EventId::new("remove-backend")));
    fixture.describe(&described);
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let input = &tree.root.children[0];
    assert_eq!(input.role, "TagInput");
    assert_eq!(input.value.as_deref(), Some("new"));
    assert_eq!(input.children[0].label.as_deref(), Some("backend"));
    assert_eq!(
        input.actions,
        vec![
            hl_extension::SemanticActionKind::Change,
            hl_extension::SemanticActionKind::Submit,
        ]
    );
}

fn markdown_preserves_bounded_source_in_semantics() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::markdown_view("# Review\n- safe <html>"));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let document = &tree.root.children[0];
    assert_eq!(document.role, "MarkdownView");
    assert_eq!(document.value.as_deref(), Some("# Review\n- safe <html>"));
    assert!(document.actions.is_empty(), "a document is readable, not actionable");
}

fn command_palette_exposes_typed_semantic_actions() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::command_palette(
        EventId::new("filter-command"),
        EventId::new("run-command"),
    ));
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let palette = &tree.root.children[0];
    assert_eq!(palette.role, "CommandPalette");
    assert_eq!(
        palette.actions,
        vec![
            hl_extension::SemanticActionKind::Change,
            hl_extension::SemanticActionKind::Submit,
        ]
    );
}

fn retiring_a_pane_discards_its_queued_interaction() {
    let mut fixture = Fixture::new();
    let pane = fixture.page.pane("pane-gone");
    let frame = Reconciliation::new().reconcile(&panel("Gone"));
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-gone".into(),
            frame,
        })
        .expect("the page is listening");
    fixture.page.tick();
    let button = descendants(&pane)
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Button>().ok())
        .expect("retained pane button");
    button.emit_clicked();
    fixture.page.retire("pane-gone");
    fixture.page.tick();
    assert!(
        fixture.recorded.borrow().is_empty(),
        "a retired pane cannot leak its queued event"
    );
}

fn retired_panes_ignore_late_frames_until_explicitly_remounted() {
    let mut fixture = Fixture::new();
    let first = fixture.page.pane("pane-reused");
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-reused".into(),
            frame: Reconciliation::new().reconcile(&panel("First generation")),
        })
        .expect("first generation frame queued");
    fixture.page.tick();
    assert!(descendants(&first).iter().any(|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text().as_str() == "First generation")
    }));

    fixture.page.retire("pane-reused");
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-reused".into(),
            frame: Reconciliation::new().reconcile(&panel("Stale generation")),
        })
        .expect("late frame queued");
    fixture.page.tick();
    assert!(
        !fixture.page.panes.contains_key("pane-reused"),
        "a late frame cannot recreate retired slot authority"
    );

    let replacement = fixture.page.pane("pane-reused");
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-reused".into(),
            frame: Reconciliation::new().reconcile(&panel("Replacement generation")),
        })
        .expect("replacement frame queued");
    fixture.page.tick();
    assert!(descendants(&replacement).iter().any(|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text().as_str() == "Replacement generation")
    }));
}

fn oversized_tree_growth_is_atomic_isolated_and_remountable() {
    let mut fixture = Fixture::new();
    fixture.describe(&Element::text("last valid interface"));
    fixture.page.tick();
    let valid_sequence = fixture.page.tree.sequence();
    let valid_nodes = fixture.page.tree.len();

    let mut oversized = Element::column();
    for index in 0..=super::TREE_NODE_LIMIT {
        oversized = oversized.child(Element::text(format!("node {index}")));
    }
    fixture.describe(&oversized);
    fixture.page.tick();
    assert_eq!(
        fixture.page.tree.sequence(),
        valid_sequence,
        "rejected growth consumes no sequence"
    );
    assert_eq!(
        fixture.page.tree.len(),
        valid_nodes,
        "rejected growth mutates no retained nodes"
    );
    assert!(
        fixture.widgets().iter().any(|widget| {
            widget
                .downcast_ref::<gtk::Label>()
                .is_some_and(|label| label.text().as_str() == "last valid interface")
        }),
        "the last valid GTK interface remains visible"
    );
    assert!(fixture.page.banner().is_visible());
    assert!(fixture.page.banner().text().contains("above the limit"));

    let mut healthy = Fixture::new();
    healthy.describe(&Element::text("independent extension"));
    healthy.page.tick();
    assert_eq!(
        healthy.page.tree.sequence(),
        1,
        "another extension owns an independent tree budget"
    );

    let slot = "bounded-pane";
    let _rejected = fixture.page.pane(slot);
    let mut large = Reconciliation::new();
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: slot.into(),
            frame: large.reconcile(&oversized),
        })
        .expect("oversized pane frame queued");
    fixture.page.tick();
    fixture.page.retire(slot);
    let replacement = fixture.page.pane(slot);
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: slot.into(),
            frame: Reconciliation::new().reconcile(&Element::text("recovered pane")),
        })
        .expect("fresh pane frame queued");
    fixture.page.tick();
    assert!(descendants(&replacement).iter().any(|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text().as_str() == "recovered pane")
    }));
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut index = 0;
    while index < found.len() {
        found.extend(offspring(&found[index]));
        index += 1;
    }
    found
}

fn retained_pane_actions_keep_their_slot() {
    let mut fixture = Fixture::new();
    let _pane = fixture.page.pane("pane-2");
    let frame = Reconciliation::new().reconcile(&panel("Pane two"));
    fixture
        .post
        .send(Delivery::FrameAt {
            slot: "pane-2".into(),
            frame,
        })
        .expect("the page is listening");
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-2").expect("pane semantics");
    let button = &tree.root.children[0].children[1];
    fixture
        .page
        .semantic_action_at(
            "pane-2",
            &hl_extension::PaneSemanticAction {
                revision: tree.revision,
                node: button.id,
                action: hl_extension::SemanticActionKind::Invoke,
                value: None,
            },
        )
        .expect("declared pane action");
    assert!(matches!(
        fixture.recorded.borrow().last(),
        Some(Signal::InteractionAt { slot, event: Event::Invoke { .. } }) if slot == "pane-2"
    ));
}

fn semantics_are_redacted_and_actions_reject_stale_revisions() {
    let mut fixture = Fixture::new();
    let described = Element::column().child(
        Element::password_entry(EventId::new("secret"))
            .prop(hl_gui::Prop::Label, hl_gui::PropValue::text("Password"))
            .prop(hl_gui::Prop::Value, hl_gui::PropValue::text("hunter2"))
            .prop(hl_gui::Prop::Secret, hl_gui::PropValue::Flag(true)),
    );
    fixture.describe(&described);
    fixture.page.tick();
    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let field = &tree.root.children[0].children[0];
    assert_eq!(field.label.as_deref(), Some("Password"));
    assert_eq!(field.value.as_deref(), Some("[redacted]"));
    assert_eq!(field.actions, vec![hl_extension::SemanticActionKind::Change]);

    fixture
        .page
        .semantic_action(&hl_extension::PaneSemanticAction {
            revision: tree.revision,
            node: field.id,
            action: hl_extension::SemanticActionKind::Change,
            value: Some("replacement".into()),
        })
        .expect("declared action");
    assert!(matches!(
        fixture.recorded.borrow().last(),
        Some(Signal::Interaction(Event::Change { .. }))
    ));
    let stale = fixture.page.semantic_action(&hl_extension::PaneSemanticAction {
        revision: tree.revision.saturating_sub(1),
        node: field.id,
        action: hl_extension::SemanticActionKind::Change,
        value: None,
    });
    assert!(matches!(stale, Err(hl_extension::HostError::Conflict(_))));
}

fn semantic_actions_are_safe_by_default_and_preserve_authored_danger() {
    let mut fixture = Fixture::new();
    let described = Element::column()
        .child(Element::button("Keep", EventId::new("keep")))
        .child(
            Element::button("Delete", EventId::new("delete"))
                .prop(hl_gui::Prop::Destructive, hl_gui::PropValue::Flag(true)),
        );
    fixture.describe(&described);
    fixture.page.tick();

    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let actions = &tree.root.children[0].children;
    assert!(!actions[0].destructive, "ordinary actions remain safe by default");
    assert!(actions[1].destructive, "authored danger reaches semantic clients");
}

fn disabled_and_hidden_controls_are_not_advertised_as_actions() {
    let mut fixture = Fixture::new();
    let described = Element::column()
        .child(
            Element::button("Unavailable", EventId::new("disabled"))
                .prop(hl_gui::Prop::Enabled, hl_gui::PropValue::Flag(false)),
        )
        .child(
            Element::button("Hidden", EventId::new("hidden"))
                .prop(hl_gui::Prop::Visible, hl_gui::PropValue::Flag(false)),
        );
    fixture.describe(&described);
    fixture.page.tick();

    let tree = fixture.page.semantics("pane-1").expect("semantic snapshot");
    let controls = &tree.root.children[0].children;
    assert_eq!(
        controls.len(),
        1,
        "hidden controls stay out of the visible semantic tree"
    );
    let disabled = &controls[0];
    assert_eq!(disabled.label.as_deref(), Some("Unavailable"));
    assert!(disabled.disabled, "disabled state remains understandable");
    assert!(
        disabled.actions.is_empty(),
        "disabled controls advertise no executable actions"
    );

    let rejected = fixture.page.semantic_action(&hl_extension::PaneSemanticAction {
        revision: tree.revision,
        node: disabled.id,
        action: hl_extension::SemanticActionKind::Invoke,
        value: None,
    });
    assert!(matches!(rejected, Err(hl_extension::HostError::Conflict(_))));
    assert!(
        fixture.recorded.borrow().is_empty(),
        "a rejected action never reaches the extension"
    );

    let button = fixture
        .widgets()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.label().as_deref() == Some("Unavailable"))
        .expect("disabled control is still visibly explained");
    assert!(!button.is_sensitive(), "GTK and semantic actionability agree");
}

fn a_structured_fault_reaches_lifecycle_on_the_toolkit_tick() {
    let (post, deliveries) = channel();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let carried = Rc::clone(&seen);
    let (_widget, mut page) = Interface::with_faults(
        deliveries,
        Rc::new(|_| {}),
        Rc::new(move |count| {
            carried.borrow_mut().push(count);
        }),
    );

    post.send(Delivery::Fault { restarts: 5 }).expect("page listening");
    assert!(
        seen.borrow().is_empty(),
        "the host thread never calls GTK lifecycle directly"
    );
    page.tick();
    assert_eq!(&*seen.borrow(), &[5]);
}

fn a_queued_frame_puts_widgets_on_the_page() {
    let mut fixture = Fixture::new();
    let empty = fixture.page.surface().len();

    fixture.describe(&panel("Containers"));
    let applied = fixture.page.tick();

    assert_eq!(applied, 1, "one queued frame, one applied delivery");
    assert!(
        fixture.page.surface().len() > empty,
        "the surface holds {} widgets",
        fixture.page.surface().len()
    );
    assert!(fixture.tagged(Tag::Button).is_some(), "the button reached the page");
    assert!(!fixture.page.banner().is_visible(), "a live extension shows no banner");
}

fn an_identical_frame_changes_nothing() {
    let mut fixture = Fixture::new();
    fixture.describe(&panel("Containers"));
    fixture.page.tick();
    let widgets = fixture.page.surface().len();
    let live = fixture.widgets().len();

    fixture.describe(&panel("Containers"));
    fixture.page.tick();

    assert_eq!(
        fixture.page.surface().len(),
        widgets,
        "no widget was created or destroyed"
    );
    assert_eq!(fixture.widgets().len(), live, "the widget tree is untouched");
}

fn a_burst_beyond_the_tick_bound_stays_queued() {
    let mut fixture = Fixture::new();
    let burst = DRAIN + 3;
    for index in 0..burst {
        fixture.describe(&panel(&format!("Containers {index}")));
    }

    let first = fixture.page.tick();
    let second = fixture.page.tick();

    assert_eq!(first, DRAIN, "a tick applies at most the bound");
    assert_eq!(second, burst - DRAIN, "the remainder was queued, not dropped");
    assert!(
        !fixture.page.banner().is_visible(),
        "every frame applied in order: {}",
        fixture.page.banner().text()
    );
}

fn a_stopped_extension_keeps_its_widgets_and_says_so() {
    let mut fixture = Fixture::new();
    fixture.describe(&panel("Containers"));
    fixture.page.tick();
    let widgets = fixture.page.surface().len();
    let live = fixture.widgets().len();
    let live_semantics = fixture.page.semantics("pane-1").expect("live semantics");

    fixture
        .post
        .send(Delivery::Loss("the socket closed".into()))
        .expect("the page is listening");
    fixture.page.tick();

    assert_eq!(
        fixture.page.surface().len(),
        widgets,
        "the surface is retained, not blanked"
    );
    assert!(fixture.widgets().len() >= live, "the last interface is still on screen");
    assert!(fixture.tagged(Tag::Button).is_some(), "including its widgets");
    assert!(fixture.page.banner().is_visible(), "the banner explains the stop");
    assert!(
        fixture.page.banner().text().contains("the socket closed"),
        "the banner says why, got {:?}",
        fixture.page.banner().text()
    );
    let faulted = fixture.page.semantics("pane-1").expect("fault semantics");
    assert_ne!(
        faulted.revision, live_semantics.revision,
        "the fault invalidates semantic observers"
    );
    let fault = faulted
        .root
        .children
        .iter()
        .find(|node| node.label.as_deref() == Some("Extension stopped"))
        .expect("the visible fault has a semantic projection");
    assert!(
        fault
            .value
            .as_deref()
            .is_some_and(|value| value.contains("socket closed"))
    );
    assert_eq!(fault.actions, vec![hl_extension::SemanticActionKind::Invoke]);
    fixture
        .page
        .semantic_action_at(
            "pane-1",
            &hl_extension::PaneSemanticAction {
                revision: faulted.revision,
                node: fault.id,
                action: hl_extension::SemanticActionKind::Invoke,
                value: None,
            },
        )
        .expect("semantic retry");
    assert_eq!(fixture.recorded.borrow().as_slice(), [Signal::Retry]);
    let pending = fixture.page.semantics("pane-1").expect("pending recovery semantics");
    assert_ne!(
        pending.revision, faulted.revision,
        "requesting recovery invalidates observers"
    );
    let pending_fault = pending
        .root
        .children
        .iter()
        .find(|node| node.id == fault.id)
        .expect("the fault remains until a fresh frame");
    assert!(
        pending_fault.disabled,
        "a pending retry cannot launch a duplicate recovery"
    );
    assert!(
        fixture
            .page
            .semantic_action_at(
                "pane-1",
                &hl_extension::PaneSemanticAction {
                    revision: pending.revision,
                    node: fault.id,
                    action: hl_extension::SemanticActionKind::Invoke,
                    value: None,
                },
            )
            .is_err(),
        "a second semantic retry fails closed"
    );
    let retry = fixture
        .widgets()
        .into_iter()
        .find(|widget| widget.has_css_class("hl-extension-retry"))
        .expect("the banner offers a retry action")
        .downcast::<gtk::Button>()
        .expect("a button");
    retry.emit_clicked();
    assert_eq!(
        fixture.recorded.borrow().as_slice(),
        [Signal::Retry],
        "the visible button cannot duplicate a pending semantic retry"
    );
    fixture.describe(&panel("Containers recovered"));
    fixture.page.tick();
    let recovered = fixture.page.semantics("pane-1").expect("recovered semantics");
    assert_ne!(
        recovered.revision, pending.revision,
        "the fresh frame invalidates pending state"
    );
    assert!(
        !fixture.page.banner().is_visible(),
        "only a valid fresh frame clears the fault"
    );
    assert!(
        recovered.root.children.iter().all(|node| node.id != fault.id),
        "the recovered pane no longer advertises the fault"
    );
}

fn a_rendered_button_reaches_the_sink() {
    let mut fixture = Fixture::new();
    fixture.describe(&panel("Containers"));
    fixture.page.tick();

    let button = fixture
        .tagged(Tag::Button)
        .expect("the button is reachable through its style class")
        .downcast::<gtk::Button>()
        .expect("a button tag builds a button");
    button.emit_clicked();
    fixture.page.tick();

    let recorded = fixture.recorded.borrow();
    assert!(
        recorded.iter().any(|signal| matches!(
            signal,
            Signal::Interaction(Event::Invoke { id, .. }) if id.as_str() == "restart"
        )),
        "the click reached the sink, got {recorded:?}"
    );
}
