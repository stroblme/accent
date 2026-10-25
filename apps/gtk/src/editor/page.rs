//! Document-page font, zoom, margins, column cap, line numbers, minimap
//! and sticky block title.

use super::{Flavour, Syntax, Tab, line_end};
use crate::{highlight, lang, platform, wrap};
use adw::prelude::*;
use gtk::{gdk, glib, graphene, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Page gutters and vertical margins at 100% zoom. [`Tab::set_page`] scales
/// them with text and column width to retain page proportions.
pub(super) const GUTTER: i32 = 48;
const TOP: i32 = 24;
const BOTTOM: i32 = 96;
/// Minimum capped column width at 100% zoom: 384 text pixels, about 52
/// GNOME-font characters. Prevent unreadably narrow percentage columns;
/// the 30% preference minimum makes it apply only below 1600 editor pixels.
pub(crate) const COLUMN_FLOOR: i32 = 480;
/// Opacity of unhovered line numbers over the background, muting the
/// scheme's gutter colour further. Match quote alpha in `highlight::restyle`.
const DIM: f64 = 0.6;

/// Compute a `percent` column cap from `available` editor pixels, with [`COLUMN_FLOOR`].
///
/// Scale the cap with zoom so it eventually fills a narrow editor, while the floor retains
/// a readable line width.
fn column_max(available: i32, percent: u32, zoom: f64) -> i32 {
    let wanted = f64::from(available) * f64::from(percent) / 100.0;
    (wanted.max(f64::from(COLUMN_FLOOR)) * zoom).round() as i32
}

// -------------------------------------------------------------------------------- line numbers

/// Digit count of the last line, sized with the widest glyph so gutter
/// width stays stable while scrolling.
fn digits(line_count: i32) -> usize {
    line_count.max(1).to_string().len()
}

/// Install dim line numbers that brighten on pointer hover.
///
/// Heading markers occupy a separate page gutter. Renderer opacity dims numbers without
/// requiring a colour API; [`numbers`] draws them. The view's per-tab font provider reaches
/// the renderer so numbering follows zoom.
pub(super) fn line_numbers(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
) -> sourceview5::GutterRenderer {
    let renderer = glib::Object::new::<numbers::Numbers>();
    renderer.set_xalign(1.0);
    renderer.set_xpad(6);
    renderer.set_visible(false);
    renderer.set_opacity(DIM);
    renderer.set_digits(digits(buffer.line_count()));

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
        move |buffer| renderer.set_digits(digits(buffer.line_count()))
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
    renderer.upcast()
}

/// Which line `renderer` last drew the caret's highlight on — what `ACCENT_BENCH_DIAG` prints
/// beside the line the caret is really on.
#[cfg(feature = "bench")]
pub(super) fn painted_cursor(renderer: &sourceview5::GutterRenderer) -> Option<u32> {
    renderer
        .downcast_ref::<numbers::Numbers>()
        .and_then(numbers::Numbers::painted_cursor)
}

/// Draw line numbers with the caret line's scheme colour and weight.
///
/// Align to the text's first baseline rather than the padded cell: comparison alignment
/// adds blank space above rows, and GtkSourceView's alignment includes that space even
/// for empty lines.
mod numbers {
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gdk, glib, graphene, pango};
    use sourceview5::prelude::*;
    use sourceview5::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    glib::wrapper! {
        pub struct Numbers(ObjectSubclass<imp::Numbers>)
            @extends sourceview5::GutterRenderer, gtk::Widget,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
    }

    impl Numbers {
        /// Make the column as wide as `digits` digits.
        pub(super) fn set_digits(&self, digits: usize) {
            if self.imp().digits.replace(digits) != digits {
                self.queue_resize();
            }
        }

        /// The line the last pass over the gutter drew as the caret's, or `None` where the caret
        /// was not among the lines it drew.
        #[cfg(feature = "bench")]
        pub(super) fn painted_cursor(&self) -> Option<u32> {
            self.imp().cursor.get()
        }
    }

    /// Extra text offset below the cell top: comparison padding, otherwise zero.
    fn blank_above(view: &gtk::TextView, line: &gtk::TextIter) -> i32 {
        let (top, _) = view.line_yrange(line);
        (view.iter_location(line).y() - top - view.pixels_above_lines()).max(0)
    }

    /// Caret-number attributes: scheme `current-line-number` foreground
    /// and bold unless explicitly disabled.
    fn caret_attributes(buffer: Option<sourceview5::Buffer>) -> pango::AttrList {
        let attributes = pango::AttrList::new();
        let Some(scheme) = buffer.and_then(|buffer| buffer.style_scheme()) else {
            return attributes;
        };
        let style = scheme.style("current-line-number");
        let colour = style
            .as_ref()
            .filter(|style| style.is_foreground_set())
            .and_then(|style| style.foreground())
            .and_then(|colour| gdk::RGBA::parse(colour.as_str()).ok());
        if let Some(c) = colour {
            let channel = |v: f32| (f64::from(v) * 65535.0) as u16;
            attributes.insert(pango::AttrColor::new_foreground(
                channel(c.red()),
                channel(c.green()),
                channel(c.blue()),
            ));
        }
        if style.is_none_or(|style| !style.is_bold_set() || style.is_bold()) {
            attributes.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        }
        attributes
    }

    /// What one pass over the gutter draws with, from its `begin` to its `end`.
    pub struct Pass {
        layout: pango::Layout,
        ink: gdk::RGBA,
        /// The caret's number's attributes, `None` while a selection is up.
        caret: Option<pango::AttrList>,
    }

    mod imp {
        use super::*;

        #[derive(Default)]
        pub struct Numbers {
            pub digits: Cell<usize>,
            pub pass: RefCell<Option<Pass>>,
            /// Last painted caret line for `ACCENT_BENCH_DIAG`, detecting stale
            /// gutters after caret movement.
            #[cfg(feature = "bench")]
            pub cursor: Cell<Option<u32>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for Numbers {
            const NAME: &'static str = "AccentLineNumbers";
            type Type = super::Numbers;
            type ParentType = sourceview5::GutterRenderer;
        }

        impl ObjectImpl for Numbers {}

        impl WidgetImpl for Numbers {
            /// As wide as the widest number the buffer can need, in the font the numbers are
            /// drawn in, plus the padding either side.
            fn measure(
                &self,
                orientation: gtk::Orientation,
                _for_size: i32,
            ) -> (i32, i32, i32, i32) {
                if orientation != gtk::Orientation::Horizontal {
                    return (0, 0, -1, -1);
                }
                let digits = self.digits.get().max(1);
                let layout = self.obj().create_pango_layout(None);
                let widest = (0..10)
                    .map(|digit| {
                        layout.set_text(&digit.to_string().repeat(digits));
                        layout.pixel_size().0
                    })
                    .max()
                    .unwrap_or(0);
                let width = widest + 2 * self.obj().xpad();
                (width, width, -1, -1)
            }
        }

        impl GutterRendererImpl for Numbers {
            fn begin(&self, lines: &sourceview5::GutterLines) {
                self.parent_begin(lines);
                let obj = self.obj();
                let selecting = lines.buffer().has_selection();
                self.pass.replace(Some(Pass {
                    layout: obj.create_pango_layout(None),
                    ink: obj.color(),
                    caret: (!selecting).then(|| caret_attributes(obj.buffer())),
                }));
                #[cfg(feature = "bench")]
                self.cursor.set(None);
            }

            fn snapshot_line(
                &self,
                snapshot: &gtk::Snapshot,
                lines: &sourceview5::GutterLines,
                line: u32,
            ) {
                let pass = self.pass.borrow();
                let Some(Pass { layout, ink, caret }) = pass.as_ref() else {
                    return;
                };
                // A line hidden inside a fold still reaches here, laid out with no height, and its
                // number would be painted on top of the header's.
                let mode = sourceview5::GutterRendererAlignmentMode::Cell;
                if lines.line_yrange(line, mode).1 <= 0 {
                    return;
                }
                let caret = caret.as_ref().filter(|_| lines.is_cursor(line));
                layout.set_text(&(line + 1).to_string());
                layout.set_attributes(caret);
                let (width, height) = layout.pixel_size();
                let (x, y) = self.obj().align_cell(line, width as f32, height as f32);
                let blank = blank_above(&lines.view(), &lines.iter_at_line(line));
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x.ceil(), y.ceil() + blank as f32));
                snapshot.append_layout(layout, ink);
                snapshot.restore();
                #[cfg(feature = "bench")]
                if caret.is_some() {
                    self.cursor.set(Some(line));
                }
            }

            fn end(&self) {
                self.pass.take();
                self.parent_end();
            }
        }
    }
}

/// Replace the font provider for views named `name`, scaled by `zoom`.
///
/// Default notes use the display-wide rule. Zoomed notes need explicit sizes because CSS
/// cannot scale an inherited size; code always names its monospace font.
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
        Flavour::Note(_) => match font.filter(|f| !f.is_empty()) {
            Some(font) => Some(font.to_string()),
            None if zoom != 1.0 => Some(default_font()),
            None => None,
        },
        _ => Some(platform::monospace_font()),
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

/// Sync `view`'s Pango context with its current style font after the style rule is installed.
///
/// GTK builds the context before the view enters a window, then updates it only for reported
/// style changes. GtkSourceView measures tab stops in `set_tab_width` using that early context.
/// If the view's font equals GTK's default, GTK may report no change, leaving tab stops and
/// wrap indents measured with the display-wide editor font (ISSUES.md).
pub(crate) fn refont(view: &impl IsA<gtk::Widget>) {
    if let Some(font) = view.create_pango_context().font_description() {
        view.pango_context().set_font_description(&font);
    }
}

/// Style a note's text, a tab's or a notebook markdown cell's: the note's tags, and the wrap of
/// the lines a fence took in or let go.
pub(crate) fn style_note(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    syntax: Syntax,
) -> (accent_core::markdown::Analysis, highlight::Offsets) {
    wrap::refence(view, || highlight::apply(buffer, syntax))
}

/// A note's tags in the theme's colours, and its heading markers hung outside the text's left
/// edge in the view's margin.
pub(crate) fn restyle_note(view: &sourceview5::View, buffer: &sourceview5::Buffer, syntax: Syntax) {
    highlight::restyle(buffer, view);
    highlight::hang(buffer, view, syntax);
}

/// Measure again what is measured in the view's font, once a new one is in: a note's hung
/// heading markers and every wrap indent.
pub(crate) fn remeasure(view: &sourceview5::View, flavour: Flavour) {
    if let Flavour::Note(syntax) = flavour
        && let Some(buffer) = view.buffer().downcast_ref::<sourceview5::Buffer>()
    {
        highlight::hang(buffer, view, syntax);
    }
    wrap::measure(view);
}

/// Zoom-scaled page margins, shared by standalone comparison views
/// ([`Tab::set_page`]).
pub(crate) fn set_margins(view: &sourceview5::View, zoom: f64) {
    let scale = |base: i32| (f64::from(base) * zoom).round() as i32;
    view.set_left_margin(scale(GUTTER));
    view.set_right_margin(scale(GUTTER));
    view.set_top_margin(scale(TOP));
    view.set_bottom_margin(scale(BOTTOM));
}

/// Per-view CSS name for a tab's font provider. GTK 4 deprecated per-widget
/// `StyleContext::add_provider` in 4.10, leaving `#name` as the hook.
/// A display-wide provider is an option if per-tab provider count matters.
pub(crate) fn next_view_name() -> String {
    thread_local! {
        static NEXT: Cell<u32> = const { Cell::new(0) };
    }
    NEXT.with(|n| {
        n.set(n.get() + 1);
        format!("accent-doc-{}", n.get())
    })
}

/// Default to the platform monospace at the document-font size; Preferences' Reset restores it.
///
/// Monospace keeps fences, tables and wikilinks aligned while the size follows the system;
/// preferences can override it.
pub fn default_font() -> String {
    let mut desc = pango::FontDescription::from_string(&platform::document_font());
    desc.set_family(&platform::monospace_family());
    desc.to_str().to_string()
}

/// Text attributes painting `colour` behind the glyphs, as a text tag's `background` does.
fn background(colour: &gdk::RGBA) -> pango::AttrList {
    let channel = |v: f32| (f64::from(v) * 65535.0).round() as u16;
    let (r, g, b) = (colour.red(), colour.green(), colour.blue());
    let attributes = pango::AttrList::new();
    attributes.insert(pango::AttrColor::new_background(
        channel(r),
        channel(g),
        channel(b),
    ));
    attributes.insert(pango::AttrInt::new_background_alpha(channel(
        colour.alpha(),
    )));
    attributes
}

/// Choose the innermost sticky-block opener, omitting one on the top
/// visible line because its text is already shown.
fn sticky_opener(top: i32, heading: Option<i32>, fence: Option<i32>) -> Option<i32> {
    [heading, fence]
        .into_iter()
        .flatten()
        .filter(|line| *line < top)
        .max()
}

/// Extract family and point size with shared defaults, scaled by `zoom` to two decimals.
///
/// Display-wide and per-tab rules use this function so missing descriptions cannot choose
/// different faces; rounding avoids floating-point noise in CSS.
pub(crate) fn font_css(name: &str, selector: &str, zoom: f64) -> String {
    let desc = pango::FontDescription::from_string(name);
    let family = desc
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(platform::monospace_family);
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
        refont(&self.view);
        self.rehang();
    }

    /// Scale text, gutters and column cap together to preserve page proportions
    /// and enough gutter space for hanging heading markers while zooming.
    fn set_page(&self, zoom: f64) {
        self.zoom.set(zoom);
        set_margins(&self.view, zoom);
        self.set_clamp();
    }

    /// Recompute the percentage column cap after allocation, resize, zoom or
    /// preference changes, once the editor width is known.
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

    /// Remeasure markers and wrap indent from the next idle, after frame-clock
    /// CSS validation updates Pango. gtk4-rs 0.11 exposes no `css_changed` vfunc.
    pub fn rehang(self: &Rc<Self>) {
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || {
                remeasure(&tab.view, tab.flavour);
                tab.map.relayout();
                // A comparison's companion is set in the same font, and takes the page with it.
                if let Some(hosted) = tab.hosted() {
                    hosted.follow_editor(true);
                }
            }
        ));
    }

    /// Toggle line numbers for all text tabs, outside the 48 px heading-marker gutter.
    pub fn set_line_numbers(&self, on: bool) {
        self.numbers.set_visible(on);
        // The pinned title starts where the text does, after the gutter the numbers widen.
        self.update_sticky();
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

    /// Replace the scrollbar with the minimap so the document does not lose
    /// width to both.
    pub fn set_minimap(&self, on: bool) {
        self.map.set_visible(on);
        let vertical = match on {
            true => gtk::PolicyType::External,
            false => gtk::PolicyType::Automatic,
        };
        self.scroller
            .set_policy(gtk::PolicyType::Automatic, vertical);
    }

    /// Scroll by `n` visible-line heights without moving the caret.
    ///
    /// GtkTextView's adjustment step is a fraction of a page, not a line. Use `line_at_y` because
    /// `iter_at_location` rejects page-gutter coordinates; y-only lookup also clamps top margins.
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

    /// Find the innermost note heading or fence enclosing the top visible line.
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

    /// Pin the enclosing block's opening line after it scrolls out of view.
    ///
    /// Note headings and fences use existing tag-toggle indices, avoiding a parse or backward
    /// scan. Source files use language-server symbols; CSV has no blocks.
    pub fn update_sticky(&self) {
        if matches!(self.flavour, Flavour::Csv(_)) {
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
            Flavour::Note(_) => self.sticky_note_line(first, top),
            Flavour::Code => {
                lang::innermost(&self.lang.symbols(), top.max(0) as u32).map(|line| line as i32)
            }
            Flavour::Csv(_) => None,
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
        // A fence's opening line is code, washed behind its text as the code under it is.
        let wash = self
            .buffer
            .tag_table()
            .lookup(highlight::CODEBLOCK)
            .filter(|tag| start.has_tag(tag))
            .and_then(|tag| tag.background_rgba());
        self.sticky
            .set_attributes(wash.as_ref().map(background).as_ref());
        // The text starts after the left gutter (fold chevrons, change bars, line numbers), at
        // the minimum width GtkTextView gives it, and the page's margin. Measured rather than read
        // off the allocation, so a gutter changing width takes the title along on the same frame.
        // The clamp centres the view in the scroller, so where that is is asked every time.
        let gutter = gtk::prelude::TextViewExt::gutter(&self.view, gtk::TextWindowType::Left);
        let border = gutter.map_or(0, |g| g.measure(gtk::Orientation::Horizontal, -1).0);
        let start = graphene::Point::new((border + self.view.left_margin()) as f32, 0.0);
        let left = self
            .view
            .compute_point(&self.sticky_bar, &start)
            .map_or(0, |point| point.x() as i32);
        self.sticky.set_margin_start(left.max(0));
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

    /// Verify missing font sizes fall back to GNOME's 11 pt, scaled by zoom. Shared CSS
    /// formatting keeps display-wide and per-tab fallbacks consistent. A missing family is the
    /// platform's monospace, which only a running libadwaita can name.
    #[test]
    fn font_css_fills_in_a_missing_size() {
        assert!(font_css("Cantarell", "#doc", 2.0).contains("font-size: 22pt"));
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
