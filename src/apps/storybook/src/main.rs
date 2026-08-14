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
const SOURCE: hl_gui::SourceId = hl_gui::SourceId::new(1);
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
    if std::env::var("STORYBOOK_LIVE").is_ok() {
        live(application, widgets, tree);
        return;
    }
    let filter = std::env::var("STORYBOOK_STORY").ok();
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
fn live(application: &gtk::Application, mut widgets: Widgets, mut tree: Tree) {
    match storybook::host(&mut widgets, &mut tree) {
        Ok(applied) => eprintln!("[storybook] applied {applied} mutations from the extension"),
        Err(fault) => {
            eprintln!("[storybook] extension failed: {fault}");
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
    if let Err(failure) = widgets.resize(SOURCE, hl_gui::Version::new(1), storybook::ROWS) {
        eprintln!("[storybook] source rejected: {failure}");
        return;
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
