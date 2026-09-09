use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;

type Errand = (Box<dyn FnOnce() + Send>, SyncSender<std::thread::Result<()>>);

pub(crate) fn on_the_toolkit_thread(scenario: impl FnOnce() + Send + 'static) -> bool {
    let Some(errands) = toolkit() else { return false };
    let (finished, outcome) = sync_channel(1);
    errands.send((Box::new(scenario), finished)).expect("toolkit thread remains available");
    match outcome.recv().expect("toolkit thread answers its errand") {
        Ok(()) => true,
        Err(panic) => panic!("on the toolkit thread: {}", said(&panic)),
    }
}

fn said(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|said| (*said).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the scenario panicked".to_owned())
}

fn toolkit() -> Option<&'static SyncSender<Errand>> {
    static TOOLKIT: OnceLock<Option<SyncSender<Errand>>> = OnceLock::new();
    TOOLKIT
        .get_or_init(|| {
            let (errands, queue) = sync_channel::<Errand>(0);
            let (entered, entry) = sync_channel(1);
            std::thread::Builder::new()
                .name("hl-gui-gtk-test-toolkit".into())
                .spawn(move || serve(&queue, &entered))
                .expect("toolkit thread starts");
            entry.recv().expect("toolkit thread reports initialization").then_some(errands)
        })
        .as_ref()
}

fn serve(queue: &Receiver<Errand>, entered: &SyncSender<bool>) {
    let drawable = gtk::init().is_ok() && gtk::gdk::Display::default().is_some();
    entered.send(drawable).expect("initialization is awaited");
    if !drawable { return; }
    while let Ok((scenario, finished)) = queue.recv() {
        let _ = finished.send(std::panic::catch_unwind(AssertUnwindSafe(scenario)));
    }
}
