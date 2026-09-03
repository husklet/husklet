//! A layout that lays children out in lines, breaking to a new one when the
//! next child does not fit — what a `gtk::Box` refuses to do and what a
//! `gtk::FlowBox` would only do for a container built as one from the start.

use std::cell::Cell;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Default)]
struct Line {
    children: Vec<(gtk::Widget, i32, i32)>,
    main: i32,
    cross: i32,
}

impl Line {
    /// A line opened by the child that did not fit the previous one.
    fn open(child: gtk::Widget, main: i32, cross: i32) -> Self {
        Self {
            children: vec![(child, main, cross)],
            main,
            cross,
        }
    }
}

pub struct Weave {
    direction: Cell<gtk::Orientation>,
    spacing: Cell<i32>,
}

impl Default for Weave {
    fn default() -> Self {
        Self {
            direction: Cell::new(gtk::Orientation::Horizontal),
            spacing: Cell::new(0),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for Weave {
    const NAME: &'static str = "HlFlow";
    type Type = Flow;
    type ParentType = gtk::LayoutManager;
}

impl ObjectImpl for Weave {}

impl LayoutManagerImpl for Weave {
    /// Height depends on width here — that is the whole point of wrapping —
    /// so GTK must measure the cross axis knowing the main one.
    fn request_mode(&self, _widget: &gtk::Widget) -> gtk::SizeRequestMode {
        match self.direction.get() {
            gtk::Orientation::Vertical => gtk::SizeRequestMode::WidthForHeight,
            _ => gtk::SizeRequestMode::HeightForWidth,
        }
    }

    fn measure(&self, widget: &gtk::Widget, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
        let spacing = self.spacing.get();
        if orientation == self.direction.get() {
            let unconstrained = self.lines(widget, -1);
            let floor = unconstrained
                .iter()
                .flat_map(|line| &line.children)
                .map(|(child, _, _)| minimum(child, self.direction.get() == gtk::Orientation::Vertical))
                .max()
                .unwrap_or(0);
            let natural = unconstrained.iter().map(|line| line.main).max().unwrap_or(0).max(floor);
            let minimum = if for_size < 0 {
                floor
            } else {
                // GTK may ask the inverse half of height-for-width (or
                // width-for-height) while checking a widget's geometry. Find
                // the narrowest main-axis extent whose wrapped cross extent
                // actually fits the supplied constraint.
                let mut low = floor;
                let mut high = natural;
                while low < high {
                    let candidate = low + (high - low) / 2;
                    if extent(&self.lines(widget, candidate), spacing) <= for_size {
                        high = candidate;
                    } else {
                        low = candidate + 1;
                    }
                }
                low
            };
            return (minimum, natural, -1, -1);
        }
        let lines = self.lines(widget, for_size);
        let stacked = extent(&lines, spacing);
        (stacked, stacked, -1, -1)
    }

    fn allocate(&self, widget: &gtk::Widget, width: i32, height: i32, _baseline: i32) {
        let spacing = self.spacing.get();
        let vertical = self.direction.get() == gtk::Orientation::Vertical;
        let room = if vertical { height } else { width };
        let mut cross = 0;
        for line in self.lines(widget, room) {
            self.line(&line, cross, vertical);
            cross += line.cross + spacing;
        }
    }
}

impl Weave {
    /// Breaks the children into lines that each fit `room` on the main axis.
    /// A negative `room` means "unconstrained", which is one line.
    fn lines(&self, widget: &gtk::Widget, room: i32) -> Vec<Line> {
        let spacing = self.spacing.get();
        let vertical = self.direction.get() == gtk::Orientation::Vertical;
        let mut lines = vec![Line::default()];
        for child in children(widget) {
            let (main, cross) = size(&child, vertical, room);
            let line = lines.last_mut().expect("a line is always open");
            let advance = if line.children.is_empty() { main } else { main + spacing };
            if room >= 0 && !line.children.is_empty() && line.main + advance > room {
                lines.push(Line::open(child, main, cross));
                continue;
            }
            line.main += advance;
            line.cross = line.cross.max(cross);
            line.children.push((child, main, cross));
        }
        lines
    }

    /// Places one line's children, each at its natural main size.
    fn line(&self, line: &Line, cross: i32, vertical: bool) {
        let spacing = self.spacing.get();
        let mut main = 0;
        for (child, extent, _) in &line.children {
            let (x, y) = if vertical { (cross, main) } else { (main, cross) };
            let (width, height) = if vertical {
                (line.cross, *extent)
            } else {
                (*extent, line.cross)
            };
            let shift = gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, y as f32));
            child.allocate(width, height, -1, Some(shift));
            main += extent + spacing;
        }
    }
}

fn size(child: &gtk::Widget, vertical: bool, room: i32) -> (i32, i32) {
    let (main, cross) = if vertical {
        (gtk::Orientation::Vertical, gtk::Orientation::Horizontal)
    } else {
        (gtk::Orientation::Horizontal, gtk::Orientation::Vertical)
    };
    let (minimum, natural, _, _) = child.measure(main, -1);
    let along = if room < 0 {
        natural
    } else {
        natural.min(room).max(minimum)
    };
    let (_, across, _, _) = child.measure(cross, along);
    (along, across)
}

fn minimum(child: &gtk::Widget, vertical: bool) -> i32 {
    let axis = if vertical {
        gtk::Orientation::Vertical
    } else {
        gtk::Orientation::Horizontal
    };
    child.measure(axis, -1).0
}

fn children(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut cursor = widget.first_child();
    while let Some(child) = cursor {
        cursor = child.next_sibling();
        if child.should_layout() {
            found.push(child);
        }
    }
    found
}

fn extent(lines: &[Line], spacing: i32) -> i32 {
    let total: i32 = lines.iter().map(|line| line.cross).sum();
    let gaps = i32::try_from(lines.len().saturating_sub(1)).unwrap_or(0) * spacing;
    total + gaps
}

glib::wrapper! {
    /// A wrapping line layout, shared by rows and columns.
    pub struct Flow(ObjectSubclass<Weave>) @extends gtk::LayoutManager;
}

impl Flow {
    /// A layout flowing along `direction`.
    #[must_use]
    pub fn new(direction: gtk::Orientation) -> Self {
        let flow: Self = glib::Object::new();
        flow.set_direction(direction);
        flow
    }

    /// The axis children advance along before a line breaks.
    #[must_use]
    pub fn direction(&self) -> gtk::Orientation {
        self.imp().direction.get()
    }

    pub fn set_direction(&self, direction: gtk::Orientation) {
        self.imp().direction.set(direction);
        self.layout_changed();
    }

    /// Space between children on a line, and between lines.
    #[must_use]
    pub fn spacing(&self) -> i32 {
        self.imp().spacing.get()
    }

    pub fn set_spacing(&self, spacing: i32) {
        self.imp().spacing.set(spacing);
        self.layout_changed();
    }
}
