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
        let reverse = !vertical && widget.direction() == gtk::TextDirection::Rtl;
        let room = if vertical { height } else { width };
        let cross_room = if vertical { width } else { height };
        let lines = self.lines(widget, room);
        let expanding = lines
            .iter()
            .filter(|line| {
                line.children.iter().any(|(child, _, _)| {
                    if vertical {
                        child.hexpands()
                    } else {
                        child.vexpands()
                    }
                })
            })
            .count();
        let expanding = i32::try_from(expanding).unwrap_or(i32::MAX);
        let spare = cross_room.saturating_sub(extent(&lines, spacing));
        let share = if expanding == 0 { 0 } else { spare / expanding };
        let mut remainder = if expanding == 0 { 0 } else { spare % expanding };
        let mut cross = 0;
        for line in lines {
            let cross_expands = line.children.iter().any(|(child, _, _)| {
                if vertical {
                    child.hexpands()
                } else {
                    child.vexpands()
                }
            });
            let bonus = if cross_expands {
                let bonus = share + i32::from(remainder > 0);
                remainder = remainder.saturating_sub(1);
                bonus
            } else {
                0
            };
            let line_cross = line.cross + bonus;
            self.line(&line, cross, line_cross, vertical, room, reverse);
            cross += line_cross + spacing;
        }
    }
}

impl Weave {
    /// Breaks the children into lines that each fit `room` on the main axis.
    /// A negative `room` means "unconstrained", which is one line.
    fn lines(&self, widget: &gtk::Widget, room: i32) -> Vec<Line> {
        let spacing = self.spacing.get();
        let vertical = self.direction.get() == gtk::Orientation::Vertical;
        let children = children(widget);
        let compact_cards = !vertical
            && (0..=600).contains(&room)
            && !children.is_empty()
            && children.iter().all(|child| child.has_css_class("hl-card"));
        let packs_cards = !vertical && children.iter().any(|child| child.has_css_class("hl-card"));
        let mut lines = vec![Line::default()];
        for child in children {
            let (mut main, mut cross) = size(&child, vertical, room, packs_cards);
            if compact_cards {
                // A compact card owns its complete row. Measure its height at
                // that final width rather than retaining the taller
                // height-for-width result from its authored packing floor.
                main = room;
                cross = child.measure(gtk::Orientation::Vertical, room).1;
            }
            let line = lines.last_mut().expect("a line is always open");
            let advance = if line.children.is_empty() { main } else { main + spacing };
            if room >= 0 && !line.children.is_empty() && (compact_cards || line.main + advance > room) {
                lines.push(Line::open(child, main, cross));
                continue;
            }
            line.main += advance;
            line.cross = line.cross.max(cross);
            line.children.push((child, main, cross));
        }
        lines
    }

    /// Places one line's children, sharing spare room among children that ask
    /// to grow just as a non-wrapping box does.
    fn line(&self, line: &Line, cross: i32, line_cross: i32, vertical: bool, room: i32, reverse: bool) {
        let spacing = self.spacing.get();
        let expanding = line
            .children
            .iter()
            .filter(|(child, _, _)| {
                if vertical {
                    child.vexpands()
                } else {
                    child.hexpands() || (room <= 600 && child.has_css_class("hl-card"))
                }
            })
            .count();
        let expanding = i32::try_from(expanding).unwrap_or(i32::MAX);
        let spare = room.saturating_sub(line.main);
        let share = if expanding == 0 { 0 } else { spare / expanding };
        let mut remainder = if expanding == 0 { 0 } else { spare % expanding };
        let equal_cards = !vertical
            && !line.children.is_empty()
            && line
                .children
                .iter()
                .all(|(child, _, _)| child.has_css_class("hl-card") && child.hexpands());
        let card_room = room.saturating_sub(
            spacing.saturating_mul(i32::try_from(line.children.len().saturating_sub(1)).unwrap_or(i32::MAX)),
        );
        let card_share = card_room / i32::try_from(line.children.len()).unwrap_or(1).max(1);
        let mut card_remainder = card_room % i32::try_from(line.children.len()).unwrap_or(1).max(1);
        let mut main = if reverse { room } else { 0 };
        for (child, extent, child_cross) in &line.children {
            let expands = if vertical {
                child.vexpands()
            } else {
                child.hexpands() || (room <= 600 && child.has_css_class("hl-card"))
            };
            let bonus = if expands {
                let bonus = share + i32::from(remainder > 0);
                remainder = remainder.saturating_sub(1);
                bonus
            } else {
                0
            };
            let extent = if equal_cards {
                let extent = card_share + i32::from(card_remainder > 0);
                card_remainder = card_remainder.saturating_sub(1);
                extent
            } else {
                extent + bonus
            };
            if reverse {
                main -= extent;
            }
            let (x, y) = if vertical { (cross, main) } else { (main, cross) };
            // A child stretches across its line only when it asks to expand on
            // that axis. In particular, a card with Height::Content must keep
            // its natural height when a diagnostic makes a peer taller.
            let cross_extent = if if vertical {
                child.hexpands()
            } else {
                child.vexpands()
            } {
                line_cross
            } else {
                *child_cross
            };
            let (width, height) = if vertical {
                (cross_extent, extent)
            } else {
                (extent, cross_extent)
            };
            let shift = gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x as f32, y as f32));
            child.allocate(width, height, -1, Some(shift));
            if reverse {
                main -= spacing;
            } else {
                main += extent + spacing;
            }
        }
    }
}

fn size(child: &gtk::Widget, vertical: bool, room: i32, packs_cards: bool) -> (i32, i32) {
    let (main, cross) = if vertical {
        (gtk::Orientation::Vertical, gtk::Orientation::Horizontal)
    } else {
        (gtk::Orientation::Horizontal, gtk::Orientation::Vertical)
    };
    let (minimum, natural, _, _) = child.measure(main, -1);
    let along = if room < 0 {
        natural
    } else if !vertical && child.hexpands() && packs_cards {
        // An expanding child has already said that its authored floor is the
        // amount needed to enter a line; `line` gives it a share of everything
        // left. A line containing cards deliberately packs all of its growing
        // children from that floor: packing an adjacent status from its
        // intrinsic width would let optional content silently change the card
        // collection's column count. Lines without cards keep natural requests
        // so wrapped text does not collapse to GTK's one-character minimum.
        minimum.min(room)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_card_collections_use_one_full_width_row_per_card() {
        if !crate::test_support::on_the_toolkit_thread(compact_card_collection_scenario) {
            eprintln!("skipped: no display connection");
            return;
        }
    }

    fn compact_card_collection_scenario() {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let flow = Flow::new(gtk::Orientation::Horizontal);
        flow.set_spacing(4);
        container.set_layout_manager(Some(flow.clone()));
        for _ in 0..3 {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            card.add_css_class("hl-card");
            card.set_size_request(260, 40);
            container.append(&card);
        }
        assert_eq!(flow.imp().lines(container.upcast_ref(), 600).len(), 3);
        assert_eq!(flow.imp().lines(container.upcast_ref(), 1_200).len(), 1);
        measured_allocate(container.upcast_ref(), 600, 140);
        let widths = children(container.upcast_ref())
            .into_iter()
            .map(|child| child.width())
            .collect::<Vec<_>>();
        assert_eq!(widths, vec![600, 600, 600]);
    }

    #[test]
    fn expanding_cards_pack_from_their_floor_before_sharing_spare_width() {
        if !crate::test_support::on_the_toolkit_thread(expanding_card_floor_scenario) {
            eprintln!("skipped: no display connection");
            return;
        }
    }

    fn expanding_card_floor_scenario() {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let flow = Flow::new(gtk::Orientation::Horizontal);
        flow.set_spacing(4);
        container.set_layout_manager(Some(flow.clone()));

        let status = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status.set_size_request(250, 40);
        status.set_hexpand(true);
        let emblem = gtk::Image::from_icon_name("dialog-error-symbolic");
        status.append(&emblem);
        let caption = gtk::Label::new(Some("Extension could not be completed."));
        caption.set_wrap(true);
        caption.set_max_width_chars(56);
        status.append(&caption);
        container.append(&status);

        for width in [346, 272] {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            card.set_size_request(width, 40);
            card.set_hexpand(true);
            container.append(&card);
        }

        let (minimum, natural, _, _) = status.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            natural > minimum,
            "fixture needs intrinsic width above its authored floor"
        );
        assert_eq!(flow.imp().lines(container.upcast_ref(), 884).len(), 1);
        measured_allocate(container.upcast_ref(), 884, 40);
        let cards = children(container.upcast_ref());
        assert!(cards
            .windows(2)
            .all(|pair| pair[0].allocation().y() == pair[1].allocation().y()));
        assert_eq!(cards.iter().map(gtk::Widget::width).sum::<i32>() + 8, 884);
    }

    #[test]
    fn expanding_card_rows_use_equal_columns_despite_unequal_content_floors() {
        if !crate::test_support::on_the_toolkit_thread(equal_card_columns_scenario) {
            eprintln!("skipped: no display connection");
            return;
        }
    }

    #[test]
    fn content_height_cards_do_not_inherit_a_taller_peers_height() {
        if !crate::test_support::on_the_toolkit_thread(content_height_card_scenario) {
            eprintln!("skipped: no display connection");
            return;
        }
    }

    fn content_height_card_scenario() {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let flow = Flow::new(gtk::Orientation::Horizontal);
        flow.set_spacing(4);
        container.set_layout_manager(Some(flow));

        for height in [120, 40] {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            card.add_css_class("hl-card");
            card.set_size_request(260, height);
            card.set_hexpand(true);
            card.set_vexpand(false);
            container.append(&card);
        }

        measured_allocate(container.upcast_ref(), 800, 120);
        let cards = children(container.upcast_ref());
        assert_eq!(cards[0].height(), 120);
        assert_eq!(cards[1].height(), 40);
    }

    fn equal_card_columns_scenario() {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let flow = Flow::new(gtk::Orientation::Horizontal);
        flow.set_spacing(4);
        container.set_layout_manager(Some(flow));
        for width in [260, 300, 280] {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
            card.add_css_class("hl-card");
            card.set_size_request(width, 40);
            card.set_hexpand(true);
            container.append(&card);
        }

        measured_allocate(container.upcast_ref(), 908, 40);
        assert_eq!(
            children(container.upcast_ref())
                .iter()
                .map(gtk::Widget::width)
                .collect::<Vec<_>>(),
            [300, 300, 300]
        );
    }

    #[test]
    fn expanding_wrapped_content_keeps_a_readable_line_basis() {
        if !crate::test_support::on_the_toolkit_thread(expanding_wrapped_content_scenario) {
            eprintln!("skipped: no display connection");
            return;
        }
    }

    fn expanding_wrapped_content_scenario() {
        for room in [1_200, 600] {
            let container = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let flow = Flow::new(gtk::Orientation::Horizontal);
            flow.set_spacing(12);
            container.set_layout_manager(Some(flow.clone()));

            let action = gtk::Button::with_label("Pin tab");
            action.set_size_request(75, 36);
            container.append(&action);

            let status = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            status.set_hexpand(true);
            status.append(&gtk::Image::from_icon_name("dialog-information-symbolic"));
            let caption = gtk::Label::new(Some("No change yet."));
            caption.set_wrap(true);
            caption.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            caption.set_hexpand(true);
            status.append(&caption);
            container.append(&status);

            let (minimum, natural, _, _) = status.measure(gtk::Orientation::Horizontal, -1);
            assert!(
                natural > minimum,
                "fixture must distinguish a word line from its glyph floor"
            );
            let lines = flow.imp().lines(container.upcast_ref(), room);
            assert_eq!(
                lines.len(),
                1,
                "{room}px unnecessarily wrapped a short status onto another row"
            );
            assert_eq!(lines[0].children[1].1, natural.min(room));

            measured_allocate(container.upcast_ref(), room, 56);
            assert!(
                caption.width() >= 90,
                "{room}px collapsed the receipt to {}px",
                caption.width()
            );
            assert!(
                status.height() <= 36,
                "{room}px produced a {}px-tall short receipt",
                status.height()
            );
        }
    }

    fn measured_allocate(widget: &gtk::Widget, width: i32, height: i32) {
        let (minimum_width, _, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
        assert!(width >= minimum_width, "fixture width {width}px is below its {minimum_width}px minimum");
        let (minimum_height, _, _, _) = widget.measure(gtk::Orientation::Vertical, width);
        assert!(height >= minimum_height, "fixture height {height}px is below its {minimum_height}px minimum");
        widget.allocate(width, height, -1, None);
    }
}
