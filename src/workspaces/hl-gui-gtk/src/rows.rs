//! A toolkit list model backed by the windowing cache.
//!
//! The model reports the source's full length while holding only a viewport of
//! rows. The toolkit asks for an item exactly when it realizes one, and that
//! call must return synchronously, so a miss yields a placeholder and schedules
//! a request rather than waiting.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use hl_gui::{
    Cell, CollectionSelection, Lookup, RowCache, RowRange, RowRequest, RowWindow, SelectedRow, SourceId, Tone,
};

/// Separates cell values within one encoded row.
pub(crate) const UNIT: char = '\u{1f}';
/// Shown while a row is on its way.
const PLACEHOLDER: &str = "…";
/// Shown once a row is known not to be coming.
const UNAVAILABLE: &str = "—";

/// Rows the model has decided it needs.
pub type Pending = Rc<RefCell<Vec<RowRequest>>>;

mod imp {
    use super::{encode, Pending, PLACEHOLDER, UNAVAILABLE};
    use std::cell::RefCell;
    use std::rc::Rc;

    use gtk::gio;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use hl_gui::{Lookup, RowCache, RowRange};

    #[derive(Default)]
    pub struct Rows {
        pub cache: RefCell<Option<Rc<RefCell<RowCache>>>>,
        pub pending: RefCell<Option<Pending>>,
        pub tick: std::cell::Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Rows {
        const NAME: &'static str = "HlGuiRows";
        type Type = super::Rows;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for Rows {}

    impl ListModelImpl for Rows {
        fn item_type(&self) -> glib::Type {
            gtk::StringObject::static_type()
        }

        /// The source's full length, not what is held. This is what makes the
        /// scrollbar describe the data rather than the cache.
        fn n_items(&self) -> u32 {
            let Some(cache) = self.cache.borrow().clone() else {
                return 0;
            };
            let length = cache.borrow().length().unwrap_or(0);
            u32::try_from(length).unwrap_or(u32::MAX)
        }

        /// Answers immediately, always. A miss schedules a request and returns
        /// a placeholder; it never blocks on the producer.
        fn item(&self, position: u32) -> Option<glib::Object> {
            let cache = self.cache.borrow().clone()?;
            let index = u64::from(position);
            let text = {
                let held = cache.borrow();
                match held.row(index) {
                    Lookup::Ready(row) => encode(row),
                    Lookup::Unavailable => UNAVAILABLE.into(),
                    Lookup::Pending | Lookup::Absent => PLACEHOLDER.into(),
                }
            };
            if text == PLACEHOLDER {
                self.schedule(&cache, index);
            }
            Some(gtk::StringObject::new(&text).upcast())
        }
    }

    impl Rows {
        /// Records what the cache wants for this position. The caller drains
        /// these and sends them; the model never performs the round trip.
        fn schedule(&self, cache: &Rc<RefCell<RowCache>>, index: u64) {
            let Some(pending) = self.pending.borrow().clone() else {
                return;
            };
            let tick = self.tick.get();
            let requests = cache.borrow_mut().observe(RowRange::new(index, RowRange::BLOCK), tick);
            pending.borrow_mut().extend(requests);
        }
    }
}

glib::wrapper! {
    /// A list model over a windowed source.
    pub struct Rows(ObjectSubclass<imp::Rows>) @implements gtk::gio::ListModel;
}

impl Rows {
    #[must_use]
    pub fn new(source: SourceId) -> Self {
        let model: Self = glib::Object::new();
        let cache = Rc::new(RefCell::new(RowCache::new(source)));
        model.imp().cache.replace(Some(cache));
        model.imp().pending.replace(Some(Rc::new(RefCell::new(Vec::new()))));
        model
    }

    fn cache(&self) -> Rc<RefCell<RowCache>> {
        self.imp().cache.borrow().clone().expect("constructed with a cache")
    }

    /// Producer identity this model is currently authorized to display.
    #[must_use]
    pub fn source(&self) -> SourceId {
        self.cache().borrow().source()
    }

    /// Requests the model has accumulated, taken for sending.
    #[must_use]
    pub fn drain(&self) -> Vec<RowRequest> {
        self.imp()
            .pending
            .borrow()
            .clone()
            .map(|pending| std::mem::take(&mut *pending.borrow_mut()))
            .unwrap_or_default()
    }

    /// Advances the clock the cache measures its deadlines against.
    pub fn tick(&self, now: u64) {
        self.imp().tick.set(now);
    }

    /// Records the source's length, which is what the scrollbar describes.
    pub fn resize(&self, version: hl_gui::Version, rows: u64) {
        let previous = self.n_items();
        self.cache().borrow_mut().resize(version, rows);
        let current = self.n_items();
        self.items_changed(0, previous, current);
    }

    /// Accepts a delivered window and repaints only the rows it covers.
    ///
    /// A window answering a cancelled request or a superseded generation is
    /// discarded here rather than reported: it is an ordinary consequence of
    /// scrolling, and the cache's own contract covers which ones are kept.
    pub fn deliver(&self, window: &RowWindow) {
        if !self.cache().borrow_mut().deliver(window) {
            return;
        }
        let start = u32::try_from(window.range.start).unwrap_or(u32::MAX);
        let count = u32::try_from(window.rows.len()).unwrap_or(u32::MAX);
        // Only the delivered band is invalidated, so unrelated rows keep their
        // widgets and the view does not flicker on every refresh.
        self.items_changed(start, count, count);
    }

    /// Drops cached rows. An absent range invalidates the whole source.
    pub fn invalidate(&self, version: hl_gui::Version, range: Option<RowRange>) {
        self.cache().borrow_mut().invalidate(version, range);
        let Some(range) = range else {
            let length = self.n_items();
            self.items_changed(0, length, length);
            return;
        };
        self.items_changed(u32::try_from(range.start).unwrap_or(u32::MAX), range.count, range.count);
    }

    /// Rows currently held, for tests and diagnostics.
    #[must_use]
    pub fn held(&self) -> usize {
        self.cache().borrow().len()
    }

    /// Whether the row at `position` is still waiting.
    #[must_use]
    pub fn is_pending(&self, position: u32) -> bool {
        matches!(self.cache().borrow().row(u64::from(position)), Lookup::Pending)
    }

    /// Resolves visible positions to producer-owned identities in one current
    /// source generation. Any placeholder or unavailable row fails closed.
    #[must_use]
    pub fn selection(&self, positions: &[u64]) -> Option<CollectionSelection> {
        let cache = self.cache();
        let cache = cache.borrow();
        let rows = positions
            .iter()
            .map(|index| match cache.row(*index) {
                Lookup::Ready(row) => Some(SelectedRow {
                    index: *index,
                    id: row.key,
                }),
                Lookup::Pending | Lookup::Unavailable | Lookup::Absent => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some(CollectionSelection {
            source: cache.source(),
            version: cache.version(),
            rows,
        })
    }
}

/// Renders one row as unit-separated cells, which is what a column factory
/// splits on when it binds.
fn encode(row: &hl_gui::Row) -> String {
    row.cells.iter().map(render).collect::<Vec<_>>().join(&UNIT.to_string())
}

fn render(cell: &Cell) -> String {
    match cell {
        Cell::Text(value) => value.clone(),
        Cell::Number(value) => format!("{value}"),
        Cell::Bytes(value) => hl_gui::ByteSize::new(i64::try_from(*value).unwrap_or(i64::MAX)).to_string(),
        Cell::Badge { label, tone } => badge(label, *tone),
        Cell::Stamp(value) => format!("{value}"),
        Cell::Empty => UNAVAILABLE.into(),
    }
}

fn badge(label: &str, tone: Tone) -> String {
    match tone {
        Tone::Positive => format!("● {label}"),
        Tone::Warning => format!("▲ {label}"),
        Tone::Danger => format!("✕ {label}"),
        Tone::Neutral | Tone::Accent => label.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_gui::{Row, Version};

    #[test]
    fn selection_resolves_producer_identity_only_for_current_materialized_rows() {
        if gtk::init().is_err() || gtk::gdk::Display::default().is_none() {
            eprintln!("skipped: no display connection");
            return;
        }
        let model = Rows::new(SourceId::new(7));
        model.resize(Version::new(3), 4);
        assert!(model.selection(&[0]).is_none(), "placeholder identity fails closed");
        assert!(model.item(1).is_some(), "realizing a row schedules its window");
        let request = model.drain().pop().expect("row request");
        model.deliver(&RowWindow {
            source: request.source,
            version: request.version,
            request: request.id,
            range: request.range,
            rows: (0..4)
                .map(|index| Row::new(900 + index, [Cell::text(format!("row-{index}"))]))
                .collect(),
        });

        let selected = model.selection(&[1]).expect("materialized row has authority");
        assert_eq!(selected.source, SourceId::new(7));
        assert_eq!(selected.version, Version::new(3));
        assert_eq!(selected.rows, vec![SelectedRow { index: 1, id: 901 }]);

        model.invalidate(Version::new(4), None);
        assert!(model.selection(&[1]).is_none(), "superseded row identity fails closed");
        assert!(model.selection(&[99]).is_none(), "absent row identity fails closed");
    }
}
