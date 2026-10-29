//! The native preview (stage 2): a note's page in one read-only text view on the editor's page,
//! its font, gutters, zoom and column.
//!
//! The page is cut into top-level blocks on a worker (`Syntax::page`). Another note goes in by
//! batches of at most [`BATCH`] each; the same note again replaces only the blocks between the
//! first and the last that changed, so what is above and below keeps its place, its pictures and
//! the selection. A block remembers where it starts (a mark), where each of its source lines
//! starts, its anchors, its links and what Copy writes for its pictures, formulas and tables.

use super::Keys;
use super::find::Find;
use super::text::{self, Facts, Href, Ink, Tags};
use super::view::{Decor, PreviewView};
use super::{code, figure, formula, table};
use crate::editor::{self, Flavour};
use accent_core::config::Config;
use accent_core::markdown::{self, Block, Click, Node};
use accent_core::note::Syntax;
use accent_core::search::Options;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use pulldown_cmark::{Event, Tag as Cm};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long one batch of a first render may hold the main loop.
const BATCH: Duration = Duration::from_millis(8);

/// The prose's size in pixels at `zoom`: the editor's document font, as the stylesheet's.
pub(super) fn font_px(zoom: f64) -> f64 {
    let font = gtk::pango::FontDescription::from_string(&editor::default_font());
    let pt = match f64::from(font.size()) / f64::from(gtk::pango::SCALE) {
        pt if pt > 0.0 => pt,
        _ => 11.0,
    };
    pt * 96.0 / 72.0 * zoom
}

/// A block of the page with the footnote numbers it shows, in order: equal ones show the same.
pub(super) struct Laid {
    pub block: Block<'static>,
    pub notes: Vec<usize>,
}

impl PartialEq for Laid {
    fn eq(&self, other: &Laid) -> bool {
        self.notes == other.notes && self.block.nodes == other.block.nodes
    }
}

/// `text`'s page, in the syntax `rel` is written in. Footnotes are numbered as pulldown-cmark's
/// HTML writer numbers them: in the order a reference or a definition first names each.
fn lay(rel: &str, text: &str) -> Vec<Laid> {
    let mut numbers: HashMap<String, usize> = HashMap::new();
    Syntax::of(rel)
        .page(text)
        .into_iter()
        .map(|block| {
            let mut notes = Vec::new();
            for node in &block.nodes {
                if let Node::Event(
                    Event::FootnoteReference(label) | Event::Start(Cm::FootnoteDefinition(label)),
                ) = node
                {
                    let next = numbers.len() + 1;
                    notes.push(*numbers.entry(label.to_string()).or_insert(next));
                }
            }
            Laid {
                block: block.into_static(),
                notes,
            }
        })
        .collect()
}

/// A block on show.
struct Shown {
    laid: Laid,
    /// Right gravity: text put in at its start goes before it, as a block inserted above does.
    mark: gtk::TextMark,
    facts: Facts,
}

/// The preview's state, which the view's controllers and the render's worker reach weakly.
pub(super) struct Page {
    pub view: PreviewView,
    pub buffer: gtk::TextBuffer,
    pub tags: Tags,
    pub ink: Rc<Ink>,
    /// The prose's size in pixels at the zoom.
    pub px: Cell<f64>,
    pub figures: figure::Figures,
    on_open: Box<dyn Fn(&str)>,
    on_tag: Box<dyn Fn(&str)>,
    clamp: adw::ClampScrollable,
    scroller: gtk::ScrolledWindow,
    pub zoom: Cell<f64>,
    /// The share of the view in percent, `None` under Full-Width Preview.
    column: Cell<Option<u32>>,
    font: RefCell<Option<gtk::CssProvider>>,
    rises: RefCell<HashMap<i32, gtk::TextTag>>,
    shown: RefCell<Vec<Shown>>,
    /// The note and the text last asked for, which an unchanged render skips (refresh T6).
    pub note: RefCell<(String, Rc<str>)>,
    /// The latest render's number: an older one's worker answer, or batch, is dropped.
    generation: Cell<u64>,
    /// Whether the latest render is all in; a scroll asked before waits in `pending`.
    settled: Cell<bool>,
    /// Whether the blocks on show are a whole page, which a render can patch.
    whole: Cell<bool>,
    pending: Cell<Option<u32>>,
    #[cfg(feature = "bench")]
    patched: Cell<(usize, usize)>,
}

pub struct Native {
    /// The editor's frame (`editor/open.rs`): an overlay over the scroller, with a zero-size area
    /// laid over it that reports the width the column is a share of.
    root: gtk::Widget,
    page: Rc<Page>,
    find: Find,
}

impl Native {
    /// See [`super::Preview::new`].
    pub fn new(
        resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        inverted: Keys,
        web: Keys,
        on_open: impl Fn(&str) + 'static,
        on_tag: impl Fn(&str) + 'static,
        on_invert: impl Fn(&str) + 'static,
    ) -> Native {
        let view = PreviewView::new();
        view.add_css_class("accent-doc");
        view.set_widget_name(&editor::next_view_name());
        let buffer = view.buffer();
        if let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() {
            editor::sync_scheme(source);
        }
        let clamp = adw::ClampScrollable::builder()
            .child(&view)
            .unit(adw::LengthUnit::Px)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&clamp)
            .build();
        crate::widgets::smooth_wheel(&scroller);
        let width = gtk::DrawingArea::builder()
            .can_target(false)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let overlay = gtk::Overlay::builder().child(&scroller).build();
        overlay.add_overlay(&width);

        let figures = figure::Figures::new(&view, resolve, inverted, web, on_invert);
        let page = Rc::new(Page {
            tags: Tags::new(&buffer.tag_table()),
            view,
            buffer,
            ink: Rc::default(),
            px: Cell::new(font_px(1.0)),
            figures,
            on_open: Box::new(on_open),
            on_tag: Box::new(on_tag),
            clamp,
            scroller,
            zoom: Cell::new(1.0),
            column: Cell::new(None),
            font: RefCell::default(),
            rises: RefCell::default(),
            shown: RefCell::default(),
            note: RefCell::new((String::new(), "".into())),
            generation: Cell::new(0),
            settled: Cell::new(true),
            whole: Cell::new(false),
            pending: Cell::new(None),
            #[cfg(feature = "bench")]
            patched: Cell::default(),
        });
        width.connect_resize(glib::clone!(
            #[weak]
            page,
            move |_, _, _| page.set_clamp()
        ));
        page.view.on_width(glib::clone!(
            #[weak]
            page,
            move |_| {
                table::set_width(&page);
                figure::resize(&page);
                formula::resize(&page);
            }
        ));
        page.view.on_copy(glib::clone!(
            #[weak]
            page,
            #[upgrade_or]
            None,
            move || page.copy()
        ));
        page.view.connect_realize(glib::clone!(
            #[weak]
            page,
            move |_| {
                page.restyle();
            }
        ));
        wire(&page);
        page.set_zoom(1.0);
        page.restyle();
        Native {
            root: overlay.upcast(),
            page,
            find: Find::default(),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    pub fn focus_target(&self) -> &gtk::Widget {
        self.page.view.upcast_ref()
    }

    /// Put `text` up, `rel` being the note it is; the same again changes nothing.
    pub fn render(&self, rel: &str, text: &str) {
        self.page.render(rel, text);
    }

    /// Bring source line `line` (1-based) to the top: the start of the element it is in; the
    /// first, the page's own top. Waits for a render still landing and a view not laid out yet.
    pub fn scroll_to_line(&self, line: u32) {
        self.page.scroll_to_line(line);
    }

    /// The theme's colours; whether the pictures' look changed with them.
    pub fn restyle(&self) -> bool {
        self.page.restyle()
    }

    pub fn forget_images(&self, then: impl FnOnce() + 'static) {
        figure::forget_images(&self.page, then);
    }

    pub fn forget_image(&self, key: &str) -> bool {
        figure::forget_image(&self.page, key)
    }

    /// Nothing is lost: there is no process to end.
    pub fn connect_lost(&self, _f: impl Fn() + 'static) {}

    /// The document zoom, shared with the editor: its font, gutters, column and the sizes the
    /// tags measure in its pixels.
    pub fn set_zoom(&self, zoom: f64) {
        self.page.set_zoom(zoom);
    }

    /// The page's width as Preferences set it: the editor's Column Width, or the whole view.
    pub fn set_column(&self, config: &Config) {
        let column = crate::export::css::column(config);
        if self.page.column.replace(column) != column {
            self.page.set_clamp();
        }
    }

    pub fn find(&self, text: &str, options: Options) {
        self.find.find(text, options);
    }

    pub fn find_next(&self) {
        self.find.next();
    }

    pub fn find_previous(&self) {
        self.find.previous();
    }

    pub fn find_clear(&self) {
        self.find.clear();
    }

    pub fn connect_found(&self, f: impl Fn(&str) + 'static) {
        self.find.connect_found(f);
    }
}

impl Drop for Native {
    /// Take the font rule off the display and free the view, as a closed editor tab does; an
    /// answer from the worker for it finds nothing to land in.
    fn drop(&mut self) {
        let page = &self.page;
        page.generation.set(page.generation.get() + 1);
        if let (Some(display), Some(provider)) =
            (gdk::Display::default(), page.font.borrow_mut().take())
        {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        editor::release(page.view.upcast_ref());
    }
}

impl Page {
    /// The text's width, which a table may not widen: less the pixel GtkTextView keeps at each
    /// line's end for the cursor (`SPACE_FOR_CURSOR`), by which a table as wide as the rest
    /// widened the page. Before the view is laid out, the column's cap.
    pub(super) fn text_width(&self) -> i32 {
        let column = match self.view.width() {
            0 => self
                .clamp
                .maximum_size()
                .min(self.scroller.width().max(600)),
            width => width,
        };
        (column - self.view.left_margin() - self.view.right_margin() - 1).max(1)
    }

    /// The tag that lowers a paintable `descent` pixels, so its baseline is the line's: GTK puts
    /// a paintable's bottom on the baseline.
    pub(super) fn rise(&self, descent: i32) -> gtk::TextTag {
        self.rises
            .borrow_mut()
            .entry(descent)
            .or_insert_with(|| {
                let tag = gtk::TextTag::builder()
                    .rise(-descent * gtk::pango::SCALE)
                    .build();
                self.buffer.tag_table().add(&tag);
                tag
            })
            .clone()
    }

    fn set_clamp(&self) {
        let max = match self.column.get() {
            Some(percent) => editor::column_max(self.scroller.width(), percent, self.zoom.get()),
            None => i32::MAX / 4,
        };
        self.clamp.set_maximum_size(max);
        self.clamp.set_tightening_threshold(max * 3 / 4);
    }

    fn set_zoom(&self, zoom: f64) {
        self.zoom.set(zoom);
        editor::set_margins(self.view.upcast_ref(), zoom);
        let name = self.view.widget_name();
        let note = Flavour::Note(Syntax::Markdown);
        editor::install_font(&self.font, note, None, zoom, &name);
        editor::refont(&self.view);
        self.px.set(font_px(zoom));
        self.tags.resize(self.px.get(), self.view.left_margin());
        self.set_clamp();
        table::set_width(self);
        figure::resize(self);
        formula::resize(self);
    }

    fn restyle(&self) -> bool {
        let dark = adw::StyleManager::default().is_dark();
        let fg = self.view.color();
        let (page, accent) = (
            crate::highlight::page(dark),
            crate::highlight::text_accent(),
        );
        self.ink.fg.set(fg);
        self.ink.page.set(page);
        self.ink.accent.set(accent);
        self.tags.restyle(&self.ink);
        self.view.set_decor(Decor {
            code: self.tags.codeblock.clone(),
            quote: self.tags.quote.clone(),
            callout: self.tags.callout.clone(),
            rule: self.tags.rule.clone(),
            surface: crate::theme::at(fg, 0.07),
            accent,
            line: crate::theme::at(fg, 0.15),
        });
        if let Some(source) = self.buffer.downcast_ref::<sourceview5::Buffer>() {
            editor::sync_scheme(source);
        }
        code::restyle(self);
        figure::restyle(self)
    }

    fn render(self: &Rc<Self>, rel: &str, text: &str) {
        let another = {
            let note = self.note.borrow();
            if note.0 == rel && &*note.1 == text {
                return;
            }
            note.0 != rel
        };
        let source: Rc<str> = text.into();
        *self.note.borrow_mut() = (rel.to_string(), source.clone());
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.settled.set(false);
        let (rel, text) = (rel.to_string(), text.to_string());
        let page = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let laid = crate::work::off_thread("preview page", move || lay(&rel, &text)).await;
            let (Some(laid), Some(page)) = (laid, page.upgrade()) else {
                return;
            };
            if page.generation.get() != generation {
                return;
            }
            match another || !page.whole.get() {
                true => page.show(laid, source, generation),
                false => {
                    page.patch(laid, &source);
                    page.settle();
                }
            }
        });
    }

    fn clear(&self) {
        for shown in self.shown.borrow_mut().drain(..) {
            self.buffer.delete_mark(&shown.mark);
        }
        self.buffer.set_text("");
    }

    /// Another note: the view cleared, then `laid` put in from the top by batches.
    fn show(self: &Rc<Self>, laid: Vec<Laid>, source: Rc<str>, generation: u64) {
        self.clear();
        self.whole.set(false);
        self.scroller.vadjustment().set_value(0.0);
        let mut laid = laid.into_iter().peekable();
        let page = Rc::downgrade(self);
        glib::idle_add_local(move || {
            let Some(page) = page.upgrade().filter(|p| p.generation.get() == generation) else {
                return glib::ControlFlow::Break;
            };
            let started = Instant::now();
            let mut end = page.buffer.end_iter();
            while started.elapsed() < BATCH {
                let Some(next) = laid.next() else { break };
                let shown = page.write(&mut end, next, &source);
                page.shown.borrow_mut().push(shown);
            }
            if laid.peek().is_some() {
                return glib::ControlFlow::Continue;
            }
            page.whole.set(true);
            page.settle();
            glib::ControlFlow::Break
        });
    }

    fn write(&self, at: &mut gtk::TextIter, laid: Laid, source: &str) -> Shown {
        let source = source.get(laid.block.bytes.clone()).unwrap_or_default();
        let (mark, facts) = text::write(self, at, &laid, source);
        Shown { laid, mark, facts }
    }

    /// The same note again: the blocks before the first and after the last changed one stay;
    /// only the ones between are written anew.
    fn patch(&self, new: Vec<Laid>, source: &str) {
        let mut shown = self.shown.borrow_mut();
        let prefix = shown
            .iter()
            .zip(&new)
            .take_while(|(s, n)| s.laid == **n)
            .count();
        let room = shown.len().min(new.len()) - prefix;
        let suffix = (shown.iter().rev().zip(new.iter().rev()))
            .take(room)
            .take_while(|(s, n)| s.laid == **n)
            .count();
        let at = |i: usize| match shown.get(i) {
            Some(s) => self.buffer.iter_at_mark(&s.mark),
            None => self.buffer.end_iter(),
        };
        let (mut start, mut end) = (at(prefix), at(shown.len() - suffix));
        self.buffer.delete(&mut start, &mut end);
        let old = shown.len() - suffix;
        for gone in shown.drain(prefix..old) {
            self.buffer.delete_mark(&gone.mark);
        }
        let written = new.len() - suffix - prefix;
        let mut new = new.into_iter().skip(prefix);
        let added: Vec<Shown> = new
            .by_ref()
            .take(written)
            .map(|laid| self.write(&mut start, laid, source))
            .collect();
        shown.splice(prefix..prefix, added);
        // The blocks below keep their text but not their place in the note.
        for (s, n) in shown[prefix + written..].iter_mut().zip(new) {
            s.laid.block.lines = n.block.lines;
            s.laid.block.bytes = n.block.bytes;
        }
        #[cfg(feature = "bench")]
        self.patched.set((prefix + suffix, written));
    }

    /// The render is in. A scroll asked meanwhile goes from an idle: GTK measures the text put
    /// in from an idle of a higher priority, so the place is measured, not estimated.
    fn settle(self: &Rc<Self>) {
        self.settled.set(true);
        if self.pending.get().is_none() {
            return;
        }
        let page = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(page) = page.upgrade()
                && let Some(line) = page.pending.take()
            {
                page.scroll_to_line(line);
            }
        });
    }

    fn scroll_to_line(self: &Rc<Self>, line: u32) {
        if !self.settled.get() || self.view.height() == 0 {
            let first = self.pending.replace(Some(line)).is_none();
            if self.settled.get() && first {
                self.when_laid_out();
            }
            return;
        }
        let at = {
            let shown = self.shown.borrow();
            match shown.partition_point(|s| s.laid.block.lines.start <= line) {
                0 => None,
                i => {
                    let s = &shown[i - 1];
                    let within = line - s.laid.block.lines.start;
                    let first = s.facts.lines.first().map_or(0, |l| l.1);
                    let offset = (s.facts.lines.iter().rev())
                        .find(|(at, _)| *at <= within)
                        .map_or(first, |l| l.1);
                    let start = self.buffer.iter_at_mark(&s.mark).offset();
                    // The page's first element is its top, margin included.
                    (i > 1 || offset > first).then(|| self.buffer.iter_at_offset(start + offset))
                }
            }
        };
        self.jump(at);
    }

    /// Scroll once the view is laid out, from an idle after its first frame, as the editor's
    /// `Tab::scroll_to_caret` waits.
    fn when_laid_out(self: &Rc<Self>) {
        let page = Rc::downgrade(self);
        self.view.add_tick_callback(move |view, _| {
            if view.height() == 0 {
                return glib::ControlFlow::Continue;
            }
            let page = page.clone();
            glib::idle_add_local_once(move || {
                if let Some(page) = page.upgrade()
                    && let Some(line) = page.pending.take()
                {
                    page.scroll_to_line(line);
                }
            });
            glib::ControlFlow::Break
        });
    }

    /// Put `at` at the top of the view at once, or the page's top for `None` (spike, gate G3).
    ///
    /// Not `scroll_to_mark`: it eases there from a place worked out before GTK measured the lines
    /// above, landing thousands of pixels off in a long note and painting every screen on the
    /// way. Set at once, the line is the view's first paragraph, which GTK keeps in place while
    /// it measures the rest; a wheel's easing under way gives way to it.
    fn jump(&self, at: Option<gtk::TextIter>) {
        let vadj = self.scroller.vadjustment();
        let Some(at) = at else {
            return vadj.set_value(vadj.lower());
        };
        let below = self.view.iter_location(&at).y() - self.view.visible_rect().y();
        let wanted = vadj.value() + f64::from(below);
        // The adjustment learns how tall text just put in is only at the next allocation; until
        // then a place in it lies past the adjustment's end, which would clamp the value short.
        let upper = vadj.upper().max(wanted + vadj.page_size());
        vadj.configure(
            wanted,
            vadj.lower(),
            upper,
            vadj.step_increment(),
            vadj.page_increment(),
            vadj.page_size(),
        );
    }

    /// The anchor `id` names (a heading's slug, a block id, a footnote), at once, with no Back.
    fn scroll_to_id(&self, id: &str) {
        let at = self.shown.borrow().iter().find_map(|s| {
            let (_, offset) = s.facts.ids.iter().find(|(name, _)| name == id)?;
            Some(self.buffer.iter_at_mark(&s.mark).offset() + offset)
        });
        if let Some(offset) = at {
            self.jump(Some(self.buffer.iter_at_offset(offset)));
        }
    }

    /// The block holding character `offset`, and `offset` within it.
    fn block_at(&self, offset: i32) -> Option<(usize, i32)> {
        let shown = self.shown.borrow();
        let start = |s: &Shown| self.buffer.iter_at_mark(&s.mark).offset();
        let i = shown
            .partition_point(|s| start(s) <= offset)
            .checked_sub(1)?;
        Some((i, offset - start(&shown[i])))
    }

    fn href_at(&self, offset: i32) -> Option<Href> {
        let (i, within) = self.block_at(offset)?;
        let shown = self.shown.borrow();
        let (_, href) = shown[i]
            .facts
            .links
            .iter()
            .find(|(range, _)| range.contains(&within))?;
        Some(href.clone())
    }

    /// Where `href` goes, by WebKit's rules (`markdown::click`): a vault file or wikilink to
    /// `on_open`, a tag to `on_tag`, a place on the page scrolled to, the web to the system.
    fn route(&self, href: &Href) -> (Click, String) {
        match href {
            Href::Open(target) => (Click::Open(target.clone()), target.clone()),
            Href::Tag(name) => (Click::Tag(name.clone()), name.clone()),
            Href::Url(dest) => {
                let base = markdown::base_uri(&self.note.borrow().0);
                match glib::Uri::resolve_relative(Some(&base), dest, glib::UriFlags::NONE) {
                    Ok(uri) => (markdown::click(&uri, &base), uri.to_string()),
                    Err(_) => (Click::Nowhere, dest.clone()),
                }
            }
        }
    }

    fn follow(&self, href: &Href) {
        let (click, uri) = self.route(href);
        match click {
            Click::Open(target) => (self.on_open)(&target),
            Click::Tag(name) => (self.on_tag)(&name),
            Click::Scroll => {
                if let Some((_, id)) = uri.split_once('#') {
                    self.scroll_to_id(&markdown::percent_decode(id));
                }
            }
            Click::Leave => {
                let parent = self.view.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(&uri).launch(
                    parent.as_ref(),
                    gio::Cancellable::NONE,
                    |result| {
                        if let Err(e) = result {
                            tracing::warn!("preview: cannot open link: {e}");
                        }
                    },
                );
            }
            Click::Nowhere => tracing::debug!("preview: a link to nowhere: {uri}"),
        }
    }

    /// The selection as the note writes it: each picture, formula, table and task box as its
    /// source. `None` with nothing selected.
    fn copy(&self) -> Option<String> {
        let (start, end) = self.buffer.selection_bounds()?;
        let text = self.buffer.slice(&start, &end, true);
        if !text.contains('\u{fffc}') {
            return Some(text.to_string());
        }
        let (i, _) = self.block_at(start.offset())?;
        let shown = self.shown.borrow();
        let block = self.buffer.iter_at_mark(&shown[i].mark);
        let before = self
            .buffer
            .slice(&block, &start, true)
            .matches('\u{fffc}')
            .count();
        let mut sources = shown[i..]
            .iter()
            .flat_map(|s| &s.facts.objects)
            .skip(before)
            .map(|o| o.source.as_str());
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '\u{fffc}' => out.push_str(sources.next().unwrap_or_default()),
                c => out.push(c),
            }
        }
        Some(out)
    }
}

/// The offset of the character at `x`, `y` of the view.
fn offset_at(view: &PreviewView, x: f64, y: f64) -> Option<i32> {
    let (bx, by) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    view.iter_at_location(bx, by).map(|at| at.offset())
}

/// The view's input: a click on a link follows it, the pointer over one is a hand, Space pages,
/// and the menu offers Copy and Select All (refresh T7).
fn wire(page: &Rc<Page>) {
    let view = &page.view;
    let click = gtk::GestureClick::builder().button(1).build();
    click.connect_released(glib::clone!(
        #[weak]
        page,
        move |gesture, _, x, y| {
            // A drag that selected text is no click on a link.
            if page.buffer.has_selection() {
                return;
            }
            if let Some(href) = offset_at(&page.view, x, y).and_then(|o| page.href_at(o)) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                page.follow(&href);
            }
        }
    ));
    view.add_controller(click);

    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion(glib::clone!(
        #[weak]
        page,
        move |_, x, y| {
            let link = offset_at(&page.view, x, y).and_then(|o| page.href_at(o));
            let cursor = if link.is_some() { "pointer" } else { "text" };
            page.view.set_cursor_from_name(Some(cursor));
        }
    ));
    view.add_controller(motion);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        view,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| {
            if key != gdk::Key::space || state.contains(gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            let pages = match state.contains(gdk::ModifierType::SHIFT_MASK) {
                true => -1,
                false => 1,
            };
            view.emit_move_cursor(gtk::MovementStep::Pages, pages, false);
            glib::Propagation::Stop
        }
    ));
    view.add_controller(keys);

    let menu = gtk::GestureClick::builder()
        .button(3)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    menu.connect_pressed(glib::clone!(
        #[weak]
        page,
        move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let items = gio::Menu::new();
            items.append(Some("Copy"), Some("clipboard.copy"));
            items.append(Some("Select All"), Some("selection.select-all"));
            let over = offset_at(&page.view, x, y);
            if let Some(item) = over.and_then(|o| figure::menu_item(&page, o)) {
                items.append_item(&item);
            }
            let at = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
            crate::widgets::popup_menu(&page.view, &items, Some(at));
        }
    ));
    view.add_controller(menu);
}

/// What a drill reads of the page.
#[cfg(feature = "bench")]
impl Native {
    pub fn settled(&self) -> bool {
        self.page.settled.get()
    }

    pub fn scroll_y(&self) -> f64 {
        self.page.scroller.vadjustment().value()
    }

    /// Blocks kept and written by the last patch.
    pub fn patched(&self) -> (usize, usize) {
        self.page.patched.get()
    }

    pub fn buffer(&self) -> gtk::TextBuffer {
        self.page.buffer.clone()
    }

    /// The pictures on the page, and how many reads they took.
    pub fn figures(&self) -> (Vec<figure::Probe>, u32) {
        (figure::probe(&self.page), figure::reads(&self.page))
    }

    /// Each formula's TeX, whether it is a display one, and its size as drawn against an inline
    /// one's.
    pub fn formulas(&self) -> Vec<(String, bool, f32)> {
        formula::probe(&self.page)
    }

    pub fn text_length(&self) -> i32 {
        self.page.buffer.char_count()
    }

    /// Each block: its lines, its text with every object as `[label]`, the named tags on its
    /// first character.
    pub fn blocks(&self) -> Vec<(std::ops::Range<u32>, String, Vec<String>)> {
        let page = &self.page;
        let shown = page.shown.borrow();
        (0..shown.len())
            .map(|i| {
                let start = page.buffer.iter_at_mark(&shown[i].mark);
                let end = shown.get(i + 1).map_or_else(
                    || page.buffer.end_iter(),
                    |s| page.buffer.iter_at_mark(&s.mark),
                );
                let mut labels = shown[i].facts.objects.iter().map(|o| o.label.as_str());
                let text: String = (page.buffer.slice(&start, &end, true).chars())
                    .map(|c| match c {
                        '\u{fffc}' => format!("[{}]", labels.next().unwrap_or("?")),
                        c => c.to_string(),
                    })
                    .collect();
                let mut tags: Vec<String> = (start.tags().iter())
                    .filter_map(|t| t.name().map(|n| n.to_string()))
                    .collect();
                tags.sort();
                (shown[i].laid.block.lines.clone(), text, tags)
            })
            .collect()
    }

    /// The text of the line at the top of the view.
    pub fn top_line(&self) -> String {
        let view = &self.page.view;
        let (start, _) = view.line_at_y(view.visible_rect().y());
        let mut end = start;
        end.forward_to_line_end();
        self.page.buffer.slice(&start, &end, true).to_string()
    }

    /// Every link's text and where a click on it goes: `open`, `tag`, `scroll`, `leave` or
    /// `nowhere`, and the address.
    pub fn links(&self) -> Vec<(String, String)> {
        let page = &self.page;
        let shown = page.shown.borrow();
        let mut out = Vec::new();
        for s in shown.iter() {
            let start = page.buffer.iter_at_mark(&s.mark).offset();
            for (range, href) in &s.facts.links {
                let at = |o: i32| page.buffer.iter_at_offset(start + o);
                let text = page.buffer.slice(&at(range.start), &at(range.end), true);
                let (click, uri) = page.route(href);
                let kind = match click {
                    Click::Open(_) => "open",
                    Click::Tag(_) => "tag",
                    Click::Scroll => "scroll",
                    Click::Leave => "leave",
                    Click::Nowhere => "nowhere",
                };
                out.push((text.to_string(), format!("{kind} {uri}")));
            }
        }
        out
    }

    /// Follow the first link to `tag`'s notes, as a click on it would; whether there was one.
    pub fn follow_tag(&self, tag: &str) -> bool {
        let href = Href::Tag(tag.to_string());
        let found = (self.page.shown.borrow().iter())
            .any(|s| s.facts.links.iter().any(|(_, h)| *h == href));
        if found {
            self.page.follow(&href);
        }
        found
    }

    /// Select All and what Copy writes for it.
    pub fn copy_all(&self) -> String {
        let (start, end) = self.page.buffer.bounds();
        self.page.buffer.select_range(&start, &end);
        let text = self.page.copy().unwrap_or_default();
        let at = self.page.buffer.start_iter();
        self.page.buffer.place_cursor(&at);
        text
    }
}
