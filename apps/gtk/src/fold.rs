//! Folding: hiding a block behind the line that opens it.
//!
//! GtkSourceView 5.20 has no folding of its own, so this is a `GtkTextTag` with `invisible` set
//! and a gutter renderer with a chevron in it. A fold hides whole lines — everything from the
//! start of the line after the header to the start of the line after the block — so the header
//! keeps its own newline and stays a row of its own, and the hidden lines leave no empty rows
//! behind them. Hiding a line's text but not its newline is what does leave one, which is what
//! the gutter's zero-height guards were written against.
//!
//! One tag for every fold in a buffer, not one per fold: `GtkTextTagTable` is a per-buffer
//! namespace and a tag per block would be a tag churned on every re-analysis. The cost is that
//! two nested folds merge into one tagged run, so unfolding the outer one also unfolds the inner
//! (ponytail: nobody has asked to keep an inner fold shut while its parent opens).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use accent_api::Fold;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use sourceview5::subclass::prelude::*;

/// The tag every folded run carries.
const TAG: &str = "fold";

/// What a click on a chevron runs. Stored behind an `Rc` so it can be cloned out of its cell
/// before it runs, the way every hook on a tab is.
type Toggle = RefCell<Option<Rc<dyn Fn(i32)>>>;
/// Width of the chevron column, and the icon drawn in it.
const WIDTH: i32 = 14;
const ICON: i32 = 12;
/// How present the chevron is: it is a control, but it sits next to every foldable line, so it
/// whispers until it is looked for. Same step back the line numbers take.
const ALPHA: f32 = 0.4;

/// Install the fold tag. Called for every flavour: a note folds its sections as a source file
/// folds its functions.
pub fn install_tag(buffer: &sourceview5::Buffer) {
    let tag = gtk::TextTag::builder().name(TAG).invisible(true).build();
    buffer.tag_table().add(&tag);
}

fn tag(buffer: &gtk::TextBuffer) -> Option<gtk::TextTag> {
    buffer.tag_table().lookup(TAG)
}

/// The first character of `line`, or the end of the buffer when it has no such line. Every fold
/// boundary is one of these: a fold hides whole lines.
fn line_start(buffer: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    match line >= 0 {
        true => buffer
            .iter_at_line(line)
            .unwrap_or_else(|| buffer.end_iter()),
        false => buffer.start_iter(),
    }
}

/// What `f` hides: the lines under its header, newlines included.
fn hidden(buffer: &gtk::TextBuffer, f: Fold) -> (gtk::TextIter, gtk::TextIter) {
    (
        line_start(buffer, f.start_line as i32 + 1),
        line_start(buffer, f.end_line as i32 + 1),
    )
}

/// Hide everything `f` covers behind its header line.
pub fn fold(buffer: &gtk::TextBuffer, f: Fold) {
    let Some(tag) = tag(buffer) else {
        return;
    };
    let (start, end) = hidden(buffer, f);
    if start >= end {
        return;
    }
    // A caret inside the block would be invisible and would type into text nobody can see, so it
    // comes out to the header line first.
    let insert = crate::editor::caret(buffer);
    if insert >= start && insert < end {
        buffer.place_cursor(&crate::editor::line_end(buffer, f.start_line as i32));
    }
    buffer.apply_tag(&tag, &start, &end);
}

/// Show the block whose header is `line` again. A no-op when it is not folded.
pub fn unfold(buffer: &gtk::TextBuffer, line: i32) {
    let Some(tag) = tag(buffer) else {
        return;
    };
    let start = line_start(buffer, line + 1);
    if !start.starts_tag(Some(&tag)) {
        return;
    }
    let mut end = start;
    end.forward_to_tag_toggle(Some(&tag));
    buffer.remove_tag(&tag, &start, &end);
}

/// Whether the block whose header is `line` is hidden right now.
pub fn is_folded(buffer: &gtk::TextBuffer, line: i32) -> bool {
    match tag(buffer) {
        Some(tag) => line_start(buffer, line + 1).starts_tag(Some(&tag)),
        None => false,
    }
}

/// Show `iter` if anything is hiding it. Every jump that moves the caret goes through this, so an
/// outline row or a search hit inside a folded block opens it rather than landing out of sight.
///
/// Two things hide text in this window: the editor's own fold, and the run a comparison collapses
/// between two hunks. A hit inside either used to land in invisible text, so both are opened here
/// — and a comparison keeps its run open by itself for as long as the caret is in it.
pub fn reveal(buffer: &gtk::TextBuffer, iter: &gtk::TextIter) {
    let hiding = [tag(buffer), buffer.tag_table().lookup(crate::diff::TAG_GAP)];
    for tag in hiding.into_iter().flatten() {
        if !iter.has_tag(&tag) {
            continue;
        }
        let (mut start, mut end) = (*iter, *iter);
        start.backward_to_tag_toggle(Some(&tag));
        end.forward_to_tag_toggle(Some(&tag));
        buffer.remove_tag(&tag, &start, &end);
    }
}

/// Show everything.
pub fn unfold_all(buffer: &gtk::TextBuffer) {
    if let Some(tag) = tag(buffer) {
        let (start, end) = buffer.bounds();
        buffer.remove_tag(&tag, &start, &end);
    }
}

/// The header lines that are folded right now, among the ones `folds` knows about.
fn folded_starts(buffer: &gtk::TextBuffer, folds: &[Fold]) -> HashSet<u32> {
    folds
        .iter()
        .map(|f| f.start_line)
        .filter(|line| is_folded(buffer, *line as i32))
        .collect()
}

/// Re-apply what is still folded after the server has re-analysed the file.
///
/// The old ranges are dropped whole and the surviving ones re-laid at their new line numbers: a
/// fold whose header was deleted simply is not in `new` and so does not come back, and one that
/// moved down three lines is re-applied where it is now. `old` is what the tab folded against.
pub fn resync(buffer: &gtk::TextBuffer, old: &[Fold], new: &[Fold]) {
    let shut = folded_starts(buffer, old);
    // Nothing is hidden, so there is nothing to lift: this runs on every refresh, 300 ms after
    // every edit, and lifting the tag is a pass over the whole buffer.
    if shut.is_empty() {
        return;
    }
    unfold_all(buffer);
    for f in new.iter().filter(|f| shut.contains(&f.start_line)) {
        fold(buffer, *f);
    }
}

/// The innermost fold covering `line`, which is the one a Fold command at the caret means.
///
/// Innermost is the latest header at or before the line: a nested block always opens after the
/// one that contains it.
pub fn containing(folds: &[Fold], line: u32) -> Option<&Fold> {
    folds
        .iter()
        .filter(|f| f.start_line <= line && line <= f.end_line)
        .max_by_key(|f| f.start_line)
}

// ---------------------------------------------------------------------------------- renderer

glib::wrapper! {
    pub struct Renderer(ObjectSubclass<imp::Renderer>)
        @extends sourceview5::GutterRenderer, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Renderer {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        let renderer: Renderer = glib::Object::new();
        renderer.set_size_request(WIDTH, -1);
        // Nothing folds until a language server says so, and an empty 14 px column beside every
        // note is 14 px of nothing. Same rule the change bars follow.
        renderer.set_visible(false);
        renderer
    }

    /// The header lines a chevron is drawn beside, and which are activatable.
    pub fn set_starts(&self, starts: HashSet<i32>) {
        self.set_visible(!starts.is_empty());
        self.imp().starts.replace(starts);
        self.queue_draw();
    }

    /// What a click on a chevron does. Weak in the caller, as every hook on a tab is.
    pub fn connect_toggle(&self, f: impl Fn(i32) + 'static) {
        *self.imp().on_toggle.borrow_mut() = Some(Rc::new(f));
    }

    /// Take the chevron's colour from the resolved theme foreground and re-look-up the icons,
    /// which is what an icon-theme change needs. Called from `Tab::restyle` and on every map.
    pub fn restyle(&self, view: &impl IsA<gtk::Widget>) {
        let fg = view.as_ref().color();
        // Held above the same floor a list marker is: a chevron nobody can find is a fold nobody
        // can open, and the gutter is the same page the markers sit on.
        let page = crate::highlight::page(adw::StyleManager::default().is_dark());
        self.imp()
            .colour
            .set(crate::highlight::dim(fg, page, ALPHA));
        if let Some(display) = gdk::Display::default() {
            let theme = gtk::IconTheme::for_display(&display);
            let look = |name| {
                theme.lookup_icon(
                    name,
                    &[],
                    ICON,
                    1,
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                )
            };
            self.imp()
                .icons
                .replace(Some((look("go-down-symbolic"), look("go-next-symbolic"))));
        }
        self.queue_draw();
    }
}

mod imp {
    use super::*;

    pub struct Renderer {
        /// Header lines, in buffer coordinates.
        pub starts: RefCell<HashSet<i32>>,
        /// Open, folded. `None` only before the first `restyle`.
        pub icons: RefCell<Option<(gtk::IconPaintable, gtk::IconPaintable)>>,
        pub colour: Cell<gdk::RGBA>,
        pub on_toggle: Toggle,
    }

    impl Default for Renderer {
        fn default() -> Self {
            Renderer {
                starts: RefCell::new(HashSet::new()),
                icons: RefCell::new(None),
                // Only ever seen if a draw beats the first `restyle`, which cannot happen: the
                // renderer is restyled before the view is mapped.
                colour: Cell::new(gdk::RGBA::TRANSPARENT),
                on_toggle: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Renderer {
        const NAME: &'static str = "AccentFoldGutter";
        type Type = super::Renderer;
        type ParentType = sourceview5::GutterRenderer;
    }

    impl ObjectImpl for Renderer {}
    impl WidgetImpl for Renderer {}

    impl GutterRendererImpl for Renderer {
        fn snapshot_line(
            &self,
            snapshot: &gtk::Snapshot,
            lines: &sourceview5::GutterLines,
            line: u32,
        ) {
            if !self.starts.borrow().contains(&(line as i32)) {
                return;
            }
            let icons = self.icons.borrow();
            let Some((open, shut)) = icons.as_ref() else {
                return;
            };
            let (y, height) =
                lines.line_yrange(line, sourceview5::GutterRendererAlignmentMode::Cell);
            // A line inside a fold has no height. Drawing on it would stack every hidden chevron
            // on the header's own row.
            if height <= 0 {
                return;
            }
            let icon = match super::is_folded(&lines.buffer(), line as i32) {
                true => shut,
                false => open,
            };
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                (WIDTH - ICON) as f32 / 2.0,
                y as f32 + (height - ICON) as f32 / 2.0,
            ));
            icon.snapshot_symbolic(
                snapshot,
                f64::from(ICON),
                f64::from(ICON),
                &[self.colour.get()],
            );
            snapshot.restore();
        }

        fn query_activatable(&self, iter: &gtk::TextIter, _area: &gdk::Rectangle) -> bool {
            self.starts.borrow().contains(&iter.line())
        }

        fn activate(
            &self,
            iter: &gtk::TextIter,
            _area: &gdk::Rectangle,
            _button: u32,
            _state: gdk::ModifierType,
            _n_presses: i32,
        ) {
            // Cloned out of the cell first: the handler reaches back into the tab, which reaches
            // back here to redraw.
            let hook = self.on_toggle.borrow().clone();
            if let Some(hook) = hook {
                hook(iter.line());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(start: u32, end: u32) -> Fold {
        Fold {
            start_line: start,
            end_line: end,
        }
    }

    #[test]
    fn containing_takes_the_innermost_block() {
        let folds = [f(0, 20), f(3, 9), f(5, 6), f(12, 15)];
        assert_eq!(containing(&folds, 5), Some(&f(5, 6)));
        assert_eq!(containing(&folds, 8), Some(&f(3, 9)));
        assert_eq!(containing(&folds, 11), Some(&f(0, 20)));
        assert_eq!(containing(&folds, 21), None);
        // The header line is inside its own fold, which is what Fold at the caret means there.
        assert_eq!(containing(&folds, 12), Some(&f(12, 15)));
    }
}
