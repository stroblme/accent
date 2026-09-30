//! The page a tab draws its text on: the font and zoom, the gutters and the column cap, the
//! line-number gutter, the minimap and the sticky block title over the top of the view.

use super::{Flavour, Tab, line_end};
use crate::{highlight, lang, wrap};
use adw::prelude::*;
use gtk::{gdk, glib, graphene, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Monospace by default, so code fences, tables and wikilinks line up. GNOME ships it with the
/// interface fonts, and `Reset` in preferences comes back here.
const DEFAULT_FAMILY: &str = "Adwaita Mono";
/// The page at 100 %: side gutters and the room above and below the text. [`Tab::set_page`]
/// scales them with the zoom along with the column, so zooming keeps the page's proportions
/// instead of squeezing the text into unchanged gutters.
pub(super) const GUTTER: i32 = 48;
const TOP: i32 = 24;
const BOTTOM: i32 = 96;
/// The narrowest the document column is ever capped at, in 100 % pixels. A percentage of a
/// narrow editor can ask for less than a line worth reading; this leaves 384 px of text between
/// the gutters, which measures ~52 characters in the GNOME document font. It only ever applies
/// below 1600 px of editor, because the preferences row's minimum is 30 %.
const COLUMN_FLOOR: i32 = 480;
/// How muted an unhovered line number is, as an opacity over the view's background. The style
/// scheme already draws the gutter in a grey of its own, so this is a step back from that rather
/// than the whole distance; 0.6 is the alpha `highlight::restyle` gives quotes.
const DIM: f64 = 0.6;

/// The clamp's maximum for a column that is `percent` of an editor `available` pixels wide.
///
/// Floored at [`COLUMN_FLOOR`] so a narrow window keeps a readable line, then scaled by the zoom
/// like the rest of the page: the percentage is of the editor at 100 %, and zooming in widens the
/// cap until it exceeds the editor and the column simply fills it, which is what the fixed 800 px
/// cap did too.
fn column_max(available: i32, percent: u32, zoom: f64) -> i32 {
    let wanted = f64::from(available) * f64::from(percent) / 100.0;
    (wanted.max(f64::from(COLUMN_FLOOR)) * zoom).round() as i32
}

// -------------------------------------------------------------------------------- line numbers

/// How many digits the last line's number needs. Every label is padded to this width, so they all
/// measure the same and the gutter cannot change width as the view scrolls.
fn digits(line_count: i32) -> usize {
    line_count.max(1).to_string().len()
}

/// A line-number gutter: every line numbered, dimmed, and lifted to full strength while the
/// pointer is in the gutter.
///
/// Headings are numbered like everything else. Their `#` markers hang in the 48 px page gutter
/// (`highlight::hang`), which is a column away from the numbers, so the two read as two margins
/// rather than as two things in one place.
///
/// Dimmed by the widget's own opacity rather than by a colour, because a gutter renderer has no
/// colour to set: composited over the view's background that is the same thing as the foreground
/// at an alpha, which is how `highlight::restyle` dims everything else. The pointer takes it back
/// to the full strength it was drawn at before, which is the style scheme's own gutter grey.
///
/// `query-data` arrives once per visible line and only has to print a number; where it is drawn
/// is [`numbers`]'s part. The renderer is a child of the view, so the per-tab `accent-doc-N` font
/// provider reaches it and the zoom follows.
pub(super) fn line_numbers(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
) -> sourceview5::GutterRendererText {
    let renderer: sourceview5::GutterRendererText =
        glib::Object::new::<numbers::Numbers>().upcast();
    renderer.set_xalign(1.0);
    renderer.set_xpad(6);
    renderer.set_visible(false);
    renderer.set_opacity(DIM);

    let width = Rc::new(Cell::new(digits(buffer.line_count())));
    renderer.set_text(&" ".repeat(width.get()));

    renderer.connect_query_data(glib::clone!(
        #[strong]
        width,
        move |renderer, lines, line| {
            // A line hidden inside a fold still reaches here and is laid out with no height, so
            // its number would be painted on top of the header's. Nothing is the right number.
            // The signal hands the lines over as a plain `GObject`, hence the cast.
            if let Some(lines) = lines.downcast_ref::<sourceview5::GutterLines>() {
                let mode = sourceview5::GutterRendererAlignmentMode::Cell;
                if lines.line_yrange(line, mode).1 <= 0 {
                    renderer.set_text("");
                    return;
                }
            }
            let width = width.get();
            renderer.set_text(&format!("{:>width$}", line + 1));
        }
    ));
    // A caret move repaints the column. GTK4 keeps a widget's render node until that widget is
    // invalidated, and moving the caret invalidates the view rather than the gutter renderer
    // inside it: the highlight the renderer draws under the caret's number stayed on the line the
    // caret had left, and nothing but the pointer entering the column — which sets the opacity —
    // ever brought it along. An edit and a scroll already relay the gutter out, so this is the one
    // thing missing.
    buffer.connect_cursor_moved(glib::clone!(
        #[weak]
        renderer,
        move |_| renderer.queue_draw()
    ));
    buffer.connect_changed(glib::clone!(
        #[weak]
        renderer,
        #[strong]
        width,
        move |buffer| {
            let wanted = digits(buffer.line_count());
            if width.replace(wanted) != wanted {
                renderer.set_text(&" ".repeat(wanted));
                renderer.queue_resize();
            }
        }
    ));

    // Disambiguated: `TextViewExt` has a `gutter` of its own.
    let gutter = sourceview5::prelude::ViewExt::gutter(view, gtk::TextWindowType::Left);
    gutter.insert(&renderer, 0);

    // Gutter-wide rather than per line: the pointer anywhere in the column lifts every number at
    // once. GTK picks the renderer itself under the pointer, but the controller goes on its
    // parent, whose `contains-pointer` covers the whole column, padding included.
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter(glib::clone!(
        #[weak]
        renderer,
        move |_, _, _| renderer.set_opacity(1.0)
    ));
    motion.connect_leave(glib::clone!(
        #[weak]
        renderer,
        move |_| renderer.set_opacity(DIM)
    ));
    gutter.add_controller(motion);
    renderer
}

/// Which line `renderer` last drew the caret's highlight on — what `ACCENT_BENCH_DIAG` prints
/// beside the line the caret is really on.
#[cfg(feature = "bench")]
pub(super) fn painted_cursor(renderer: &sourceview5::GutterRendererText) -> Option<u32> {
    renderer
        .downcast_ref::<numbers::Numbers>()
        .and_then(numbers::Numbers::painted_cursor)
}

/// The line numbers, drawn level with the first line of their text rather than at the top of the
/// line's cell.
///
/// The two differ where a comparison lays blank space above a line to keep it beside its partner
/// (`diff::pad`): GtkSourceView aligns a renderer in the whole cell, blank included, so the number
/// sat beside the blank, or beside the "⋯ N unchanged lines" button over it. `alignment-mode`
/// `first` with a centred `yalign` would move the numbers of lines with text, but it takes an
/// empty line's cell whole, and a blank line is the commonest line in a note.
mod numbers {
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{glib, graphene};
    use sourceview5::subclass::prelude::*;

    glib::wrapper! {
        pub struct Numbers(ObjectSubclass<imp::Numbers>)
            @extends sourceview5::GutterRendererText, sourceview5::GutterRenderer, gtk::Widget,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
    }

    impl Numbers {
        /// The line the last pass over the gutter drew as the caret's, or `None` where the caret
        /// was not among the lines it drew.
        #[cfg(feature = "bench")]
        pub(super) fn painted_cursor(&self) -> Option<u32> {
            use gtk::subclass::prelude::ObjectSubclassIsExt;
            self.imp().cursor.get()
        }
    }

    // The bindings make only `GutterRenderer` subclassable. `GutterRendererText` is derivable in C
    // and adds no virtual methods of its own, its class being `GutterRendererClass` and padding,
    // so the parent's class setup is all it needs.
    unsafe impl IsSubclassable<imp::Numbers> for sourceview5::GutterRendererText {}

    /// How far below the top of its cell a line's text starts, beyond the view's own spacing: the
    /// padding a comparison gave it, and 0 anywhere else.
    fn blank_above(view: &gtk::TextView, line: &gtk::TextIter) -> i32 {
        let (top, _) = view.line_yrange(line);
        (view.iter_location(line).y() - top - view.pixels_above_lines()).max(0)
    }

    mod imp {
        use super::*;
        #[cfg(feature = "bench")]
        use std::cell::Cell;

        #[derive(Default)]
        pub struct Numbers {
            /// Which line this renderer last drew as the caret's. Read by `ACCENT_BENCH_DIAG`,
            /// where the point is that a gutter nothing invalidated still says the line the
            /// caret has left.
            #[cfg(feature = "bench")]
            pub cursor: Cell<Option<u32>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Numbers {
            const NAME: &'static str = "AccentLineNumbers";
            type Type = super::Numbers;
            type ParentType = sourceview5::GutterRendererText;
        }

        impl ObjectImpl for Numbers {}
        impl WidgetImpl for Numbers {}

        impl GutterRendererImpl for Numbers {
            /// Once per pass over the gutter, before any line is drawn: the caret's line as this
            /// pass sees it, which is the one the parent paints its background under.
            #[cfg(feature = "bench")]
            fn begin(&self, lines: &sourceview5::GutterLines) {
                self.cursor
                    .set((lines.first()..=lines.last()).find(|&line| lines.is_cursor(line)));
                self.parent_begin(lines);
            }

            fn snapshot_line(
                &self,
                snapshot: &gtk::Snapshot,
                lines: &sourceview5::GutterLines,
                line: u32,
            ) {
                let blank = blank_above(&lines.view(), &lines.iter_at_line(line));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(0.0, blank as f32));
                self.parent_snapshot_line(snapshot, lines, line);
                snapshot.restore();
            }
        }
    }
}

/// Name the font `zoom` scales for the views called `name`, replacing the provider from last time.
///
/// At the default zoom and with no font of its own a note needs no provider at all: the
/// display-wide document font rule already says exactly the right thing. Zooming has to name a
/// font anyway, because CSS has no way to scale a size it cannot see. Code names its font every
/// time: the display-wide rule installed for prose is the GNOME *document* font, and a source
/// file wants the monospace one instead.
pub(crate) fn install_font(
    slot: &RefCell<Option<gtk::CssProvider>>,
    flavour: Flavour,
    font: Option<&str>,
    zoom: f64,
    name: &str,
) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    if let Some(old) = slot.borrow_mut().take() {
        gtk::style_context_remove_provider_for_display(&display, &old);
    }
    let family = match flavour {
        Flavour::Note => match font.filter(|f| !f.is_empty()) {
            Some(font) => Some(font.to_string()),
            None if zoom != 1.0 => Some(default_font()),
            None => None,
        },
        _ => Some(
            adw::StyleManager::default()
                .monospace_font_name()
                .to_string(),
        ),
    };
    if let Some(family) = family {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&font_css(&family, &format!("#{name}"), zoom));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *slot.borrow_mut() = Some(provider);
    }
}

/// The page's margins at `zoom`: the side gutters and the room above and below the text, scaled
/// with it as [`Tab::set_page`] explains. A comparison of two texts that are not files lays its
/// two views out on the same page.
pub(crate) fn set_margins(view: &sourceview5::View, zoom: f64) {
    let scale = |base: i32| (f64::from(base) * zoom).round() as i32;
    view.set_left_margin(scale(GUTTER));
    view.set_right_margin(scale(GUTTER));
    view.set_top_margin(scale(TOP));
    view.set_bottom_margin(scale(BOTTOM));
}

/// A per-view CSS name, so the font override can be one provider per tab.
///
/// ponytail: `#name` is the only per-widget CSS hook GTK 4 still offers — `StyleContext` and its
/// `add_provider` are deprecated since 4.10 — and the font is a global preference, so one
/// display-wide provider would do. Swap for that if the provider count ever matters.
pub(crate) fn next_view_name() -> String {
    thread_local! {
        static NEXT: Cell<u32> = const { Cell::new(0) };
    }
    NEXT.with(|n| {
        n.set(n.get() + 1);
        format!("accent-doc-{}", n.get())
    })
}

/// The editor's default font: Adwaita Mono at the size of the GNOME document font, and what
/// `main::install_document_font` puts on every editor until a preference overrides it.
///
/// The family is ours and the size still follows the system. DESIGN.md used to take the document
/// font whole on the grounds that notes are prose, but a vault is prose with code fences, tables
/// and wikilinks in it, and none of those line up in a proportional face.
pub fn default_font() -> String {
    let mut desc =
        pango::FontDescription::from_string(&adw::StyleManager::default().document_font_name());
    desc.set_family(DEFAULT_FAMILY);
    desc.to_str().to_string()
}

/// Which opener a sticky block title shows for the line at the top of the view: the innermost of
/// the two candidates, and none at all where the only one is the top line itself, which the
/// reader can already see.
fn sticky_opener(top: i32, heading: Option<i32>, fence: Option<i32>) -> Option<i32> {
    [heading, fence]
        .into_iter()
        .flatten()
        .filter(|line| *line < top)
        .max()
}

/// Family and point size of a font description, with our own defaults where it is silent, scaled
/// by `zoom`. Rounded to two decimals so stepping the zoom does not write `12.100000000000001pt`.
///
/// The one place either is decided. `main::install_document_font` writes the display-wide rule
/// through this too, at zoom 1.0: the extraction used to be written out a second time there with a
/// different fallback family, so a description with no family of its own would have produced two
/// different faces.
pub(crate) fn font_css(name: &str, selector: &str, zoom: f64) -> String {
    let desc = pango::FontDescription::from_string(name);
    let family = desc
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(|| DEFAULT_FAMILY.to_string());
    let size = match desc.size() as f64 / pango::SCALE as f64 {
        pt if pt > 0.0 => pt,
        _ => 11.0,
    };
    let size = (size * zoom * 100.0).round() / 100.0;
    format!("{selector} {{ font-family: \"{family}\"; font-size: {size}pt; }}")
}

impl Tab {
    /// `font` of `None` follows the GNOME document font that `main` installs for every editor;
    /// `zoom` scales whichever of the two applies, and only this tab's document.
    pub fn set_font(self: &Rc<Self>, font: Option<&str>, zoom: f64) {
        self.set_page(zoom);
        install_font(
            &self.font,
            self.flavour,
            font,
            zoom,
            &self.view.widget_name(),
        );
        self.rehang();
    }

    /// Scale the page with the text. Zoom used to touch the font alone, so a zoomed-in column
    /// held fewer characters between gutters that stayed 48 px wide, and the heading markers,
    /// which `highlight::hang` measures against the left margin, ran out of gutter to hang in the
    /// way h5 and h6 already do. Scaling the gutters and the clamp together with the font keeps
    /// the page proportional, so zooming reads as moving closer rather than as a narrower column.
    fn set_page(&self, zoom: f64) {
        self.zoom.set(zoom);
        set_margins(&self.view, zoom);
        self.set_clamp();
    }

    /// Cap the column at its share of the editor's current width. Called on every resize as well
    /// as on a zoom or a preference change, because the share is of a width nothing reports until
    /// the window has been laid out.
    pub(super) fn set_clamp(&self) {
        // The scroller's own width, not the horizontal adjustment's page size: with the view as
        // the scrollable child that page size *is* the clamped column, so it would feed back.
        let available = self.scroller.width();
        let max = match self.comparing.borrow().is_some() {
            // Beside another pane the column has no width to spare, so the cap comes off.
            true => i32::MAX / 4,
            false => column_max(available, self.column.get(), self.zoom.get()),
        };
        self.clamp.set_maximum_size(max);
        // The 3:4 the fixed clamp had (600 of 800): under it the child simply takes the width it
        // is given, so a window too narrow for the cap loses no text to the gutters.
        self.clamp.set_tightening_threshold(max * 3 / 4);
    }

    /// The document column as a percentage of the editor's width, from preferences.
    pub fn set_column_width(&self, percent: u32) {
        self.column.set(percent);
        self.set_clamp();
    }

    /// Re-measure the hanging heading markers and the wrap indents from the next idle. A CSS font
    /// change only reaches the view's pango context once the frame clock has validated the style,
    /// and gtk4-rs 0.11 exposes no `css_changed` vfunc to hang this off.
    pub fn rehang(self: &Rc<Self>) {
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || {
                // Only a note has markers hanging in the gutter.
                if tab.flavour.is_note() {
                    highlight::hang(&tab.buffer, &tab.view);
                }
                wrap::measure(&tab.view);
                // A comparison's companion is set in the same font, and takes the page with it.
                if let Some(compare) = tab.comparison() {
                    compare.follow_editor(true);
                }
            }
        ));
    }

    /// Numbers in the left gutter, outside the 48 px page gutter the heading markers hang in. The
    /// preference decides for every text tab, code as well as prose.
    pub fn set_line_numbers(&self, on: bool) {
        self.numbers.set_visible(on);
    }

    /// Whether the line numbers are on, and the width the gutter gave them. Only
    /// `ACCENT_BENCH_NUMBERS` reads it.
    #[cfg(feature = "bench")]
    pub fn line_numbers(&self) -> (bool, i32) {
        (self.numbers.is_visible(), self.numbers.width())
    }

    /// Whether the sticky block title is up. Only `ACCENT_BENCH_COMPARE=page:` reads it.
    #[cfg(feature = "bench")]
    pub fn sticky_shown(&self) -> bool {
        self.sticky_bar.is_visible()
    }

    /// Which line the gutter drew the caret's highlight on when it last painted, 0-based. Beside
    /// the caret's own line this says whether the column is following it; `ACCENT_BENCH_DIAG`
    /// prints the pair.
    #[cfg(feature = "bench")]
    pub fn gutter_cursor(&self) -> Option<u32> {
        painted_cursor(&self.numbers)
    }

    /// The minimap stands in for the scrollbar rather than sitting next to it, which is what
    /// VS Code's code map does and what keeps the document column from losing width twice.
    pub fn set_minimap(&self, on: bool) {
        self.map.set_visible(on);
        let vertical = match on {
            true => gtk::PolicyType::External,
            false => gtk::PolicyType::Automatic,
        };
        self.scroller
            .set_policy(gtk::PolicyType::Automatic, vertical);
    }

    /// Scroll the viewport by `n` lines, leaving the caret where it is. The adjustment's own
    /// `step_increment` is a tenth of a page in GtkTextView rather than a line, so the height
    /// comes from the first visible line instead.
    ///
    /// `line_at_y` and not `iter_at_location`: the latter answers with whether the position is
    /// *over text*, and buffer x 0 is the page gutter at every scroll position, so it returned
    /// nothing and this scrolled by nothing. `line_at_y` takes the y alone and clamps, which also
    /// covers the top of the document, where y is the negative of the top margin.
    pub fn scroll_lines(&self, n: i32) {
        let (first, _) = self.view.line_at_y(self.view.visible_rect().y());
        // The display line's height and not the paragraph's: `iter_location` measures the caret
        // at that position, so a wrapped line still steps one screen row at a time.
        let height = self.view.iter_location(&first).height();
        if height <= 0 {
            return;
        }
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(adjustment.value() + f64::from(n * height));
    }

    /// The note's own answer to what the top of the view is inside: the nearest heading or the
    /// fence the reader is inside, whichever is lower down.
    fn sticky_note_line(&self, first: gtk::TextIter, top: i32) -> Option<i32> {
        // From the *end* of the top line, so a heading or a fence opening on that line is found
        // and then discarded by `sticky_opener` for being on screen already, rather than passed
        // over in favour of the one above it.
        let from = line_end(&self.buffer, first.line());
        let table = self.buffer.tag_table();
        let previous = |name: &str| {
            let tag = table.lookup(name)?;
            let mut at = from;
            at.backward_to_tag_toggle(Some(&tag)).then(|| at.line())
        };
        let heading = highlight::HEADING_TAGS
            .iter()
            .filter_map(|name| previous(name))
            .max();
        let fence = table.lookup(highlight::CODEBLOCK).and_then(|tag| {
            // Only from inside the block: below it the nearest toggle is its closing one, which
            // is a block the reader has already left.
            let mut at = from;
            if !at.has_tag(&tag) || !at.backward_to_tag_toggle(Some(&tag)) {
                return None;
            }
            at.starts_tag(Some(&tag)).then(|| at.line())
        });
        sticky_opener(top, heading, fence)
    }

    /// Pin the opening line of whatever block the top of the view is inside above the view, or
    /// take it away again. VS Code's sticky scroll, and it answers the same question: what is
    /// this, now that its first line has gone off the top.
    ///
    /// In a note a block is a markdown heading or a fenced code block, which is exactly what the
    /// styling pass has already marked on the buffer — so the answer is two tag-toggle searches
    /// through the buffer's own index rather than a second parse or a walk back through the
    /// lines. In a source file it is the innermost symbol the language server named, which is the
    /// same question asked of the only thing that knows a language's structure. A CSV has no
    /// blocks at all.
    pub fn update_sticky(&self) {
        if self.flavour == Flavour::Csv {
            return;
        }
        // The other column of a comparison has no such bar, and one over the editor's first row
        // alone puts the two first rows out of level.
        if self.comparing.borrow().is_some() {
            self.sticky_bar.set_visible(false);
            return;
        }
        // Before the view is allocated its visible rect is empty and `line_at_y` answers with
        // whatever line the layout happens to be at, which pinned a heading over a note that was
        // at its very top. The resize hook recomputes once there is a height.
        if self.view.height() == 0 {
            self.sticky_bar.set_visible(false);
            return;
        }
        let (first, _) = self.view.line_at_y(self.view.visible_rect().y());
        let top = first.line();
        let line = match self.flavour {
            Flavour::Note => self.sticky_note_line(first, top),
            Flavour::Code => {
                lang::innermost(&self.lang.symbols(), top.max(0) as u32).map(|line| line as i32)
            }
            Flavour::Csv => None,
        };
        let Some(line) = line else {
            self.sticky_bar.set_visible(false);
            return;
        };
        let Some(start) = self.buffer.iter_at_line(line) else {
            return;
        };
        let end = line_end(&self.buffer, line);
        self.sticky
            .set_text(self.buffer.text(&start, &end, false).trim_end());
        // The clamp centres the view in the scroller, so where the text column starts is not
        // something the bar can be told once. The page gutter goes on top of it.
        let origin = graphene::Point::zero();
        let left = self
            .view
            .compute_point(&self.sticky_bar, &origin)
            .map_or(0, |point| point.x() as i32);
        self.sticky
            .set_margin_start((left + self.view.left_margin()).max(0));
        self.sticky_bar.set_visible(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sticky title shows the innermost block that has actually gone off the top.
    #[test]
    fn a_sticky_title_shows_the_innermost_block_above_the_view() {
        assert_eq!(sticky_opener(30, Some(4), None), Some(4), "a heading alone");
        assert_eq!(
            sticky_opener(30, Some(4), Some(20)),
            Some(20),
            "the fence inside the section wins"
        );
        assert_eq!(
            sticky_opener(30, Some(40), None),
            None,
            "a heading below the view is not around it"
        );
        assert_eq!(
            sticky_opener(4, Some(4), None),
            None,
            "the line itself is already on screen"
        );
        assert_eq!(
            sticky_opener(30, None, None),
            None,
            "plain prose pins nothing"
        );
    }

    #[test]
    fn column_max_is_a_share_of_the_editor_with_a_floor_under_it() {
        // The user's maximised window: 1920 less the sidebar, so the default lands within a
        // couple of dozen pixels of the 800 px the fixed clamp used to give it.
        assert_eq!(column_max(1639, 50, 1.0), 820);
        assert_eq!(column_max(1639, 100, 1.0), 1639, "all of it is allowed");
        assert_eq!(
            column_max(700, 50, 1.0),
            COLUMN_FLOOR,
            "a narrow editor keeps a readable line instead of a sliver"
        );
        assert_eq!(column_max(1639, 50, 2.0), 1639, "the zoom scales the page");
    }

    #[test]
    fn font_css_scales_the_point_size_by_the_zoom() {
        let css = font_css("Cantarell 11", "#doc", 1.0);
        assert!(css.contains("font-family: \"Cantarell\""), "{css}");
        assert!(css.contains("font-size: 11pt"), "{css}");
        assert!(
            font_css("Cantarell 11", "#doc", 1.5).contains("font-size: 16.5pt"),
            "a zoom multiplies the size"
        );
        assert!(
            font_css("Cantarell 11", "#doc", 1.1).contains("font-size: 12.1pt"),
            "and is rounded, not written out in full binary"
        );
    }

    /// A description with no size of its own falls back to GNOME's 11 pt, zoom included, and one
    /// with no family at all to the family the editor is written in. The display-wide rule goes
    /// through the same function, so a second fallback here would be a second face there.
    #[test]
    fn font_css_fills_in_a_missing_size() {
        assert!(font_css("Cantarell", "#doc", 2.0).contains("font-size: 22pt"));
        let css = font_css("11", "#doc", 1.0);
        assert!(css.contains("font-family: \"Adwaita Mono\""), "{css}");
    }

    /// The gutter is as wide as the longest number it will ever print, and never zero wide.
    #[test]
    fn gutter_width_follows_the_line_count() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(1), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(1000), 4);
    }
}
