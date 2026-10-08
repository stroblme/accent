//! The popup's widgets: the rows, and the selected row's documentation beside them, in a popover
//! over the text that never takes the keyboard.
//!
//! The popover is parented to the view like the signature's: not autohiding, so it neither grabs
//! the pointer nor the keyboard, and nothing in it can be focused, so the caret stays in the text
//! while the list is walked with the arrows the view hands over (`editor::keys`). A click on a row
//! is taken by the row itself before the list could select it and move the focus.
//!
//! GTK 4.22 presents a popover at the size it has when it is pointed somewhere and does not shrink
//! it with its content afterwards (ISSUES.md), so every change of size is followed by pointing it
//! again ([`present`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};

use super::rank::MAX_ROWS;

/// The widest the documentation reads, in characters: narrower than a hover, as it sits beside a
/// list rather than on its own.
const DOC_WIDTH: i32 = 48;

/// What the CSS (`build::install_chrome_css`) gives the popover's contents and a row on their
/// left, and the room a kind icon and the gap after it take: how far a row's label sits right of
/// the popover's edge, which is how far the popover goes left for the label to stand under what
/// was typed.
pub(crate) const CONTENTS_PADDING: i32 = 4;
pub(crate) const ROW_PADDING: i32 = 6;
const ICON_ROOM: i32 = 16 + 6;

/// A row's padding above and below its text, and the least its text may take, also the CSS's:
/// what the list's height is worked out from before a row has been laid out to measure.
pub(crate) const ROW_PADDING_Y: i32 = 3;
pub(crate) const ROW_TEXT_MIN: i32 = 20;

/// One row as the list draws it.
pub(super) struct Row {
    pub label: String,
    /// The label's characters what was typed matched, by character index.
    pub bold: Vec<u32>,
    pub detail: Option<String>,
    /// `None` hides the kind icon: a list of plain words has nothing for one to tell apart.
    pub icon: Option<&'static str>,
}

pub(super) struct Popup {
    popover: gtk::Popover,
    store: gio::ListStore,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    rows: gtk::ScrolledWindow,
    pane: gtk::Box,
    doc_scroll: gtk::ScrolledWindow,
    doc: gtk::Label,
    /// The editor's font at the tab's zoom, which every label is set in: CSS does not carry it
    /// from the view into a popover.
    font: Rc<RefCell<Option<pango::FontDescription>>>,
    /// The widest the list has been since the popup came up: typing narrows the rows, and a
    /// popup that shrank with them would jump under the caret.
    widest: Rc<Cell<i32>>,
    /// The frame callback measuring the rows as laid out, while one is waiting for a frame.
    measure: Rc<RefCell<Option<gtk::TickCallbackId>>>,
}

impl Popup {
    /// The popup over `view`. `accept` is told the row clicked, `selected` that the selection
    /// moved, by key or otherwise.
    pub(super) fn new(
        view: &impl IsA<gtk::Widget>,
        accept: impl Fn(u32) + 'static,
        selected: impl Fn() + 'static,
    ) -> Popup {
        let font: Rc<RefCell<Option<pango::FontDescription>>> = Rc::default();
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::builder()
            .model(&store)
            .autoselect(false)
            .can_unselect(true)
            .build();
        selection.connect_selected_notify(move |_| selected());
        let list = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory(Rc::new(accept), font.clone()))
            .can_focus(false)
            .focusable(false)
            .build();
        list.add_css_class("navigation-sidebar");
        let rows = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .can_focus(false)
            .child(&list)
            .build();

        let doc = gtk::Label::builder()
            .use_markup(true)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(DOC_WIDTH)
            .xalign(0.0)
            .yalign(0.0)
            .build();
        doc.add_css_class("doc");
        let doc_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .propagate_natural_width(true)
            .can_focus(false)
            .child(&doc)
            .build();
        let pane = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pane.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        pane.append(&doc_scroll);
        pane.set_visible(false);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&rows);
        content.append(&pane);
        // Start-aligned, so the popover's left edge goes where it points rather than its middle.
        let popover = gtk::Popover::builder()
            .autohide(false)
            .has_arrow(false)
            .can_focus(false)
            .position(gtk::PositionType::Bottom)
            .halign(gtk::Align::Start)
            .child(&content)
            .build();
        popover.add_css_class("accent-completion");
        popover.set_parent(view);
        Popup {
            popover,
            store,
            selection,
            list,
            rows,
            pane,
            doc_scroll,
            doc,
            font,
            widest: Rc::default(),
            measure: Rc::default(),
        }
    }

    pub(super) fn is_shown(&self) -> bool {
        self.popover.is_mapped()
    }

    /// Show `rows` under `at`, a rectangle in the view's widget coordinates: the start of what
    /// the rows complete. `selected` is the row to keep selected, if any. Never with no rows:
    /// an empty popover is presented 0 px tall, which GDK refuses with a critical.
    pub(super) fn show(&self, at: gdk::Rectangle, rows: Vec<Row>, selected: Option<u32>) {
        if rows.is_empty() {
            return self.hide();
        }
        let font = self
            .popover
            .parent()
            .and_then(|view| view.pango_context().font_description());
        self.doc
            .set_attributes(font.as_ref().map(font_attrs).as_ref());
        self.font.replace(font);
        // Sized before it is presented, which is the size it will have.
        self.estimate();
        let icons = rows.iter().any(|row| row.icon.is_some());
        let objects: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.store.splice(0, self.store.n_items(), &objects);
        self.select(selected);
        let inset = CONTENTS_PADDING + ROW_PADDING + if icons { ICON_ROOM } else { 0 };
        self.popover.set_offset(-inset, 0);
        self.popover.set_pointing_to(Some(&at));
        if !self.popover.is_visible() {
            self.popover.popup();
        }
        self.measure();
    }

    /// [`MAX_ROWS`] rows tall, a row being the font's line and the CSS's padding: right for the
    /// first frame, before any row has been laid out.
    fn estimate(&self) {
        let font = self.font.borrow();
        let metrics = self.popover.pango_context().metrics(font.as_ref(), None);
        let text = (metrics.height() + pango::SCALE - 1) / pango::SCALE;
        let row = text.max(ROW_TEXT_MIN) + 2 * ROW_PADDING_Y;
        set_tall(&self.rows, &self.doc_scroll, row * MAX_ROWS as i32);
    }

    /// On the next frame, once the rows are laid out: [`MAX_ROWS`] of them tall as they are, and
    /// never narrower than the list has been, presented again if either changed.
    fn measure(&self) {
        if let Some(waiting) = self.measure.take() {
            waiting.remove();
        }
        let (popover, rows, doc, widest, measure) = (
            self.popover.clone(),
            self.rows.clone(),
            self.doc_scroll.clone(),
            self.widest.clone(),
            self.measure.clone(),
        );
        let id = self.list.add_tick_callback(move |list, _| {
            let Some(row) = list.first_child().filter(|row| row.height() > 0) else {
                return glib::ControlFlow::Continue;
            };
            let (_, tall, _, _) = row.measure(gtk::Orientation::Vertical, row.width());
            let mut changed = set_tall(&rows, &doc, tall * MAX_ROWS as i32);
            if rows.width() > widest.get() {
                widest.set(rows.width());
                rows.set_size_request(rows.width(), -1);
                changed = true;
            }
            if changed {
                present(&popover);
            }
            measure.take();
            glib::ControlFlow::Break
        });
        self.measure.replace(Some(id));
    }

    pub(super) fn hide(&self) {
        if let Some(waiting) = self.measure.take() {
            waiting.remove();
        }
        if self.popover.is_visible() {
            self.popover.popdown();
        }
        self.pane.set_visible(false);
        self.widest.set(0);
        self.rows.set_size_request(-1, -1);
    }

    /// Point the popup at `at` again, the text having scrolled under it.
    pub(super) fn point(&self, at: gdk::Rectangle) {
        self.popover.set_pointing_to(Some(&at));
    }

    pub(super) fn len(&self) -> u32 {
        self.store.n_items()
    }

    pub(super) fn selected(&self) -> Option<u32> {
        Some(self.selection.selected()).filter(|&i| i != gtk::INVALID_LIST_POSITION)
    }

    pub(super) fn select(&self, row: Option<u32>) {
        self.selection
            .set_selected(row.unwrap_or(gtk::INVALID_LIST_POSITION));
        if let Some(row) = row {
            self.list.scroll_to(row, gtk::ListScrollFlags::NONE, None);
        }
    }

    /// The selected row's documentation as Pango markup, or `None` to put the pane away. Either
    /// changes the popup's size, so it is presented again.
    pub(super) fn set_doc(&self, markup: Option<&str>) {
        if let Some(markup) = markup {
            self.doc.set_markup(markup);
        }
        self.pane.set_visible(markup.is_some());
        present(&self.popover);
    }

    /// The documentation pane's text, while it shows.
    #[cfg(feature = "bench")]
    pub(super) fn doc(&self) -> Option<String> {
        self.pane.is_visible().then(|| self.doc.text().to_string())
    }

    /// The rows' labels as they read, first to last: what a drill prints.
    #[cfg(feature = "bench")]
    pub(super) fn labels(&self) -> Vec<String> {
        (0..self.store.n_items())
            .filter_map(|i| self.store.item(i).and_downcast::<glib::BoxedAnyObject>())
            .map(|row| row.borrow::<Row>().label.clone())
            .collect()
    }
}

impl Drop for Popup {
    /// A popover parented by hand stays parented until it is unparented by hand.
    fn drop(&mut self) {
        self.popover.unparent();
    }
}

/// Present `popover` again where it points, which is what fits it to its content.
fn present(popover: &gtk::Popover) {
    let (pointed, at) = popover.pointing_to();
    if pointed && popover.is_visible() {
        popover.set_pointing_to(Some(&at));
    }
}

/// Cap the list and the documentation beside it at `tall` pixels; whether that changed it.
fn set_tall(rows: &gtk::ScrolledWindow, doc: &gtk::ScrolledWindow, tall: i32) -> bool {
    let changed = rows.max_content_height() != tall;
    if changed {
        rows.set_max_content_height(tall);
        doc.set_max_content_height(tall);
    }
    changed
}

/// A label's attributes setting it in `font`.
fn font_attrs(font: &pango::FontDescription) -> pango::AttrList {
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrFontDesc::new(font));
    attrs
}

/// A row's widgets, built once per row the list recycles: the kind icon, the label, and the
/// detail, dimmed. A press on one accepts it; binding only ever sets what the row shows.
fn factory(
    accept: Rc<dyn Fn(u32)>,
    font: Rc<RefCell<Option<pango::FontDescription>>>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        item.set_focusable(false);
        item.set_activatable(false);
        let icon = gtk::Image::new();
        icon.add_css_class("kind");
        let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        let detail = gtk::Label::builder()
            .xalign(1.0)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(32)
            .build();
        detail.add_css_class("detail");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&icon);
        row.append(&label);
        row.append(&detail);
        // Claimed on the press, so the list never selects the row on its own, which would also
        // move the keyboard into it.
        let click = gtk::GestureClick::new();
        let accept = accept.clone();
        click.connect_pressed(glib::clone!(
            #[weak]
            item,
            move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                accept(item.position());
            }
        ));
        row.add_controller(click);
        item.set_child(Some(&row));
    });
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(object), Some(row)) = (
            item.item().and_downcast::<glib::BoxedAnyObject>(),
            item.child(),
        ) else {
            return;
        };
        let data = object.borrow::<Row>();
        let icon = row.first_child().and_downcast::<gtk::Image>();
        let label = icon
            .as_ref()
            .and_then(|i| i.next_sibling())
            .and_downcast::<gtk::Label>();
        let detail = label
            .as_ref()
            .and_then(|l| l.next_sibling())
            .and_downcast::<gtk::Label>();
        let (Some(icon), Some(label), Some(detail)) = (icon, label, detail) else {
            return;
        };
        let font = font.borrow();
        icon.set_visible(data.icon.is_some());
        icon.set_icon_name(data.icon);
        label.set_text(&data.label);
        label.set_attributes(Some(&bold(&data.label, &data.bold, font.as_ref())));
        detail.set_text(data.detail.as_deref().unwrap_or(""));
        detail.set_attributes(font.as_ref().map(font_attrs).as_ref());
        detail.set_visible(data.detail.is_some());
    });
    factory
}

/// `label` in `font`, bold over its characters at `at`, which Pango wants as byte ranges.
fn bold(label: &str, at: &[u32], font: Option<&pango::FontDescription>) -> pango::AttrList {
    let attrs = font.map(font_attrs).unwrap_or_default();
    for (i, (byte, c)) in label.char_indices().enumerate() {
        if at.binary_search(&(i as u32)).is_ok() {
            let mut weight = pango::AttrInt::new_weight(pango::Weight::Bold);
            weight.set_start_index(byte as u32);
            weight.set_end_index((byte + c.len_utf8()) as u32);
            attrs.insert(weight);
        }
    }
    attrs
}
