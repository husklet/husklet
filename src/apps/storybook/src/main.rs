//! Renders the component catalogue.
//!
//! With `STORYBOOK_SHOT=<path>` the window snapshots itself to a PNG and exits,
//! so the catalogue can be reviewed without an interactive session.

use gtk::prelude::*;
use hl_gui::{Renderer, Theme, Tree};
use hl_gui_gtk::Surface as Widgets;
use storybook::Catalogue;

mod capture;

const APP_ID: &str = "dev.hlgui.storybook";
/// Fetch rounds the storybook plays before presenting; enough to fill a view.
const ROUNDS: u64 = 4;
const WIDTH: i32 = 1100;
const HEIGHT: i32 = 1400;

fn main() -> gtk::glib::ExitCode {
    let application = gtk::Application::builder().application_id(APP_ID).build();
    application.connect_startup(|_| hl_gui_gtk::style::install(&Theme::dark()));
    application.connect_activate(present);
    application.run_with_args::<&str>(&[])
}

fn present(application: &gtk::Application) {
    let mut widgets = Widgets::new();
    let mut tree = Tree::new();
    let filter = std::env::var("STORYBOOK_STORY").ok();
    if let Ok(mode) = std::env::var("STORYBOOK_LIVE") {
        live(application, widgets, tree, &mode, filter);
        return;
    }
    let (_, frame) = Catalogue::selected(filter.as_deref());
    if let Err(failure) = tree.apply(&frame, &mut widgets) {
        eprintln!("[storybook] catalogue rejected: {failure}");
        return;
    }
    serve(&mut widgets);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(widgets.widget()));
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("hl-gui component catalogue")
        .default_width(WIDTH)
        .default_height(HEIGHT)
        .child(&scroll)
        .build();

    eprintln!(
        "[storybook] {} stories, {} widgets",
        Catalogue::STORIES.len(),
        widgets.len()
    );
    capture::Shot::schedule(&window);
    window.present();
}

/// Renders whatever the reference extension sends over a real socket.
///
/// Nothing about the interface is known here: it arrives as mutations from
/// another thread speaking the protocol, which is the same path an extension
/// running in its own container takes.
fn live(application: &gtk::Application, mut widgets: Widgets, mut tree: Tree, mode: &str, filter: Option<String>) {
    let served = match mode {
        // The whole component library, described remotely, so every component
        // is shown to survive the wire rather than only the ones an extension
        // happens to use.
        "catalogue" => storybook::catalogue(&mut widgets, &mut tree, filter),
        _ => storybook::host(&mut widgets, &mut tree),
    };
    match served {
        Ok(applied) => eprintln!("[storybook] applied {applied} mutations over the socket"),
        Err(fault) => {
            eprintln!("[storybook] producer failed: {fault}");
            return;
        }
    }
    window(application, &widgets);
}

/// Places a rendered surface in a window and shows it.
fn window(application: &gtk::Application, widgets: &Widgets) {
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(widgets.widget()));
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("hl-gui component catalogue")
        .default_width(WIDTH)
        .default_height(HEIGHT)
        .child(&scroll)
        .build();
    capture::Shot::schedule(&window);
    window.present();
}

/// Plays the producer for the catalogue's table: declare the length, then
/// answer whatever windows the view asks for.
fn serve(widgets: &mut Widgets) {
    for source in storybook::sources() {
        let Err(failure) = widgets.resize(source, hl_gui::Version::new(1), storybook::ROWS) else {
            continue;
        };
        // A source no table is bound to is ordinary when only part of the
        // catalogue was asked for.
        if !matches!(failure, hl_gui_gtk::Failure::Unbound(_)) {
            eprintln!("[storybook] source rejected: {failure}");
        }
    }
    for round in 0..ROUNDS {
        let requests = widgets.requests(round);
        if requests.is_empty() {
            return;
        }
        answer(widgets, &requests);
    }
}

/// Delivers one round of windows.
fn answer(widgets: &mut Widgets, requests: &[hl_gui::RowRequest]) {
    for request in requests {
        let Err(failure) = widgets.rows(&storybook::answer(request)) else {
            continue;
        };
        eprintln!("[storybook] window rejected: {failure}");
    }
}
