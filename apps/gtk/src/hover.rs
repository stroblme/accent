//! Show language-server hover text and all diagnostics at the pointer position, in a popover of
//! our own over the view, as the completion popup is: libadwaita draws the two alike.
//!
//! Information and hints have no visible messages elsewhere, so the hover includes them.
//! It is a Pango label: markdown bold, italic and monospace survive; headings become bold lines,
//! links retain their text, and lists lose bullets.
//!
//! It keeps GtkSourceView's hover behaviour, which it replaced: asked once the pointer has rested
//! on a word for [`DELAY`], up while the pointer is on that word or in the popover, whose text a
//! wheel scrolls, and gone on a key press, a scroll of the text, the caret moving, the view losing
//! focus, or the pointer leaving both.
//!
//! Over a diagnostic the server has fixes for, a Quick Fix… button under the text opens the menu
//! `Alt+Return` opens (`fixes.rs`).

use crate::editor::Tab;
use crate::lang::{self, Editor};
use accent_api::debug::Evaluated;
use accent_api::{CodeAction, Pos, Task};
use gtk::{gdk, glib, graphene, pango};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

/// Maximum hover/signature width in characters, keeping long signatures readable.
pub const WIDTH: i32 = 80;

/// Minimum hover width; narrow edge space should shift the popup rather than wrap every few words.
const NARROWEST: i32 = 30;

/// Pointer rest before a hover is asked for, GtkSourceView's default.
const DELAY: Duration = Duration::from_millis(500);

/// How far the text sits inside the hover's frame (CSS in `build::install_chrome_css`): as far as
/// a completion row's, so the two read alike and the text starts where the hovered word does.
pub(crate) const INSET_X: i32 =
    crate::completion::CONTENTS_PADDING + crate::completion::ROW_PADDING;
pub(crate) const INSET_Y: i32 =
    crate::completion::CONTENTS_PADDING + crate::completion::ROW_PADDING_Y;

/// Return hover width in characters and height in pixels for the available window space.
///
/// `room` is what the window leaves the text right of the hovered word; width clamps to
/// [`NARROWEST`]..=[`WIDTH`]. Height is half the window, ensuring room above or below the
/// target; overflow scrolls.
fn bounds((room, window): (i32, i32), char_width: f64) -> (i32, i32) {
    let chars = match char_width > 0.0 {
        true => (f64::from(room) / char_width).floor() as i32,
        false => WIDTH,
    };
    (chars.clamp(NARROWEST, WIDTH), window / 2)
}

/// Measure [`bounds`] from the hovered word's start in `view`, in the view's font, which the
/// hover's label inherits.
fn limits(view: &sourceview5::View, start: &gtk::TextIter) -> (i32, i32) {
    // Unbounded off screen, where nothing is hovered anyway.
    let Some(root) = view.root() else {
        return (WIDTH, -1);
    };
    let at = view.iter_location(start);
    let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, at.x(), at.y());
    let x = view
        .compute_point(&root, &graphene::Point::new(x as f32, y as f32))
        .map_or(0, |point| point.x() as i32);
    // GtkLabel counts `max-width-chars` in the wider of these two, in Pango units; whole pixels
    // would let a 9.6 px character count as 9 and the hover run past the window.
    let metrics = view.pango_context().metrics(None, None);
    let units = metrics
        .approximate_char_width()
        .max(metrics.approximate_digit_width());
    let char_width = f64::from(units) / f64::from(pango::SCALE);
    // The frame's padding and 1 px border on the right.
    let room = root.width() - x - INSET_X - 1;
    bounds((room, root.height()), char_width)
}

/// Convert `md` to escaped Pango markup so server text cannot break the hover label.
pub fn markup_of(md: &str) -> String {
    let mut out = String::new();
    let escaped = |out: &mut String, text: &str| out.push_str(&glib::markup_escape_text(text));

    for event in Parser::new(md) {
        match event {
            Event::Text(text) => escaped(&mut out, &text),
            // Inline code and a fenced block are the same thing to a label: monospace. The block
            // keeps its own newlines, which is what makes it a block.
            Event::Code(text) => {
                out.push_str("<tt>");
                escaped(&mut out, &text);
                out.push_str("</tt>");
            }
            Event::Start(Tag::CodeBlock(_)) => out.push_str("<tt>"),
            Event::End(TagEnd::CodeBlock) => out.push_str("</tt>\n"),
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            // A heading is a bold line: the label has one text size and nothing to grade.
            Event::Start(Tag::Heading { .. }) => out.push_str("<b>"),
            Event::End(TagEnd::Heading(_)) => out.push_str("</b>\n"),
            // The blank line between paragraphs is the only structure a label can show.
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),
            Event::SoftBreak | Event::HardBreak | Event::Rule => out.push('\n'),
            // Links keep their text and lose their target; lists lose their bullets; raw HTML,
            // images, tables and footnotes are dropped whole.
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Labels for diagnostic severities in the hover.
fn severity_label(severity: accent_api::Severity) -> &'static str {
    match severity {
        accent_api::Severity::Error => "Error",
        accent_api::Severity::Warning => "Warning",
        accent_api::Severity::Info => "Information",
        accent_api::Severity::Hint => "Hint",
    }
}

/// Format a diagnostic's severity, message and source as markup.
fn diagnostic_markup(item: &accent_api::Diagnostic) -> String {
    let mut line = format!(
        "<b>{}</b>: {}",
        severity_label(item.severity),
        glib::markup_escape_text(&item.message)
    );
    if let Some(source) = &item.source {
        line.push_str(&format!(
            " <span alpha=\"60%\">({})</span>",
            glib::markup_escape_text(source)
        ));
    }
    line
}

/// What the hovered name at `start` holds in the paused program, `self.items` on its `items`,
/// as it is asked for; `None` unless a program is paused in a file its debugger runs.
pub(crate) fn value_of(tab: &Rc<Tab>, start: &gtk::TextIter) -> Option<(String, Task<Evaluated>)> {
    let mut from = *start;
    from.set_line_offset(0);
    let mut to = *start;
    if !to.ends_line() {
        to.forward_to_line_end();
    }
    let expression = crate::debug::expression_at(&from.text(&to), start.line_offset() as usize)?;
    let asked = (tab.lang.hooks()?.evaluate)(tab, expression.clone())?;
    Some((expression, asked))
}

/// `expression = value` in monospace, the value's type dimmed after it.
fn value_markup(expression: &str, value: &Evaluated) -> String {
    let said = format!("{expression} = {}", value.result);
    let mut line = format!("<tt>{}</tt>", glib::markup_escape_text(&said));
    if let Some(kind) = &value.kind {
        let kind = glib::markup_escape_text(kind);
        line.push_str(&format!(" <span alpha=\"60%\">{kind}</span>"));
    }
    line
}

/// The hover's markup at `start` in `editor`: the paused program's value of the name there, the
/// server's answer, then every diagnostic at that position. `None` with none of them, and while
/// the completion popup, which is about the same text and would sit under it, is up.
pub(crate) async fn markup_at(editor: Rc<dyn Editor>, start: gtk::TextIter) -> Option<String> {
    if editor.completion().is_some_and(|s| s.is_shown()) {
        return None;
    }
    let pos = lang::pos_of(&start);
    let doc = editor.document();
    let vault = doc.vault()?;
    // Asked first, so that it is out while the server is asked.
    let value = editor.clone().value(&start);
    editor.clone().flush().await;
    let answer = vault
        .hover(&doc.key(), editor.to_document(pos)?)
        .await
        .ok()
        .flatten();
    let value = match value {
        Some((expression, asked)) => asked.await.ok().map(|v| value_markup(&expression, &v)),
        None => None,
    };
    let items = editor.diagnostics();
    let here = crate::diagnostics::at(&items, pos);
    tracing::debug!(
        "hover for {} at {pos:?}: answer {}, {} diagnostics",
        doc.key(),
        answer.is_some(),
        here.len()
    );
    if value.is_none() && answer.is_none() && here.is_empty() {
        return None;
    }
    let mut markup: Vec<String> = value.into_iter().collect();
    markup.extend(answer.map(|h| markup_of(&h.text)));
    markup.extend(here.into_iter().map(diagnostic_markup));
    Some(markup.join("\n"))
}

/// What the server offers over the first diagnostic at `pos` in the view, which the Quick Fix…
/// button stands for: nothing where the editor takes no fixes or there is no diagnostic, and
/// nothing it fails to answer.
pub(crate) async fn fixes_at(editor: Rc<dyn Editor>, pos: Pos) -> Vec<CodeAction> {
    let range = {
        let items = editor.diagnostics();
        crate::diagnostics::at(&items, pos).first().map(|d| d.range)
    };
    let doc = editor.document();
    let (Some(range), Some(vault), true) = (range, doc.vault(), editor.offers_fixes()) else {
        return Vec::new();
    };
    let (Some(from), Some(to)) = (
        editor.to_document(range.start),
        editor.to_document(range.end),
    ) else {
        return Vec::new();
    };
    let range = accent_api::Range {
        start: from,
        end: to,
    };
    vault
        .code_actions(&doc.key(), range)
        .await
        .unwrap_or_default()
}

// ------------------------------------------------------------------------------------- popover

/// One view's hover: the popover over it, and what it was asked about.
struct Hover {
    /// Weak: the editor owns the view, which owns this.
    editor: Weak<dyn Editor>,
    popover: gtk::Popover,
    scroller: gtk::ScrolledWindow,
    label: gtk::Label,
    /// Quick Fix…, shown where the server has fixes for the diagnostic hovered.
    fix: gtk::Button,
    /// Where those fixes are, as a buffer offset.
    fixes: Cell<Option<i32>>,
    /// Whether the pointer is over the view, and whether in the popover.
    over_view: gtk::EventControllerMotion,
    over_popover: gtk::EventControllerMotion,
    /// The pointer's last place over the view, in its widget coordinates.
    pointer: Cell<Option<(f64, f64)>>,
    /// The pointer resting, counted down to asking.
    settle: RefCell<Option<glib::SourceId>>,
    /// The question out, dropped (and so cancelled at the server) when the hover goes.
    asking: RefCell<Option<glib::JoinHandle<()>>>,
    /// The word asked about or shown: its start's offset and its box in buffer coordinates.
    word: Cell<Option<(i32, gdk::Rectangle)>>,
}

/// Give an editor's view its hover. Called by [`crate::lang::attach`] and for each notebook code
/// cell.
pub fn install(editor: &Rc<dyn Editor>) {
    let view = editor.view();
    let label = gtk::Label::builder()
        .use_markup(true)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .xalign(0.0)
        .build();
    // Scrolled past its bound rather than cut off: GTK fits a popover into the room beside the
    // text by shrinking it, and a label given less than its height lost its last lines mid-line,
    // which is most of a long note's preview.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .child(&label)
        .build();
    // Over the line, as GtkSourceView put it, start-aligned and pulled left by its inset so the
    // text starts where the word does; never autohiding, so the view keeps the keyboard.
    // Never taking the keyboard from the view, which would take the hover down.
    let fix = gtk::Button::builder()
        .label("Quick Fix…")
        .halign(gtk::Align::Start)
        .focus_on_click(false)
        .can_focus(false)
        .visible(false)
        .build();
    fix.add_css_class("flat");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&scroller);
    content.append(&fix);
    let popover = gtk::Popover::builder()
        .autohide(false)
        .has_arrow(false)
        .can_focus(false)
        .position(gtk::PositionType::Top)
        .halign(gtk::Align::Start)
        .child(&content)
        .build();
    popover.add_css_class("accent-hover");
    popover.set_offset(-INSET_X, 0);
    let hover = Rc::new(Hover {
        editor: Rc::downgrade(editor),
        popover,
        scroller,
        label,
        fix,
        fixes: Cell::new(None),
        over_view: gtk::EventControllerMotion::new(),
        over_popover: gtk::EventControllerMotion::new(),
        pointer: Cell::new(None),
        settle: RefCell::default(),
        asking: RefCell::default(),
        word: Cell::new(None),
    });
    hover.watch(view);
    // The view's child only while it is on screen: a GtkTextView being disposed takes out every
    // child it did not put in itself, and over a popover it never stops trying. The map handler
    // is the one strong hold on the hover, let go with the view's other handlers.
    if view.is_mapped() {
        hover.popover.set_parent(view);
    }
    view.connect_map(move |view| {
        if hover.popover.parent().is_none() {
            hover.popover.set_parent(view);
        }
    });
}

impl Hover {
    fn watch(self: &Rc<Self>, view: &sourceview5::View) {
        self.over_view.connect_motion(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_, x, y| me.moved(x, y)
        ));
        self.over_view.connect_enter(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_, x, y| me.moved(x, y)
        ));
        self.over_view.connect_leave(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.left()
        ));
        view.add_controller(self.over_view.clone());
        self.over_popover.connect_leave(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.left()
        ));
        self.popover.add_controller(self.over_popover.clone());
        self.fix.connect_clicked(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.fix()
        ));

        // Any key goes to the text, so the hover goes. Captured, ahead of the view's own
        // bindings, which take a typed character for good.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = me)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, _, _| {
                me.dismiss();
                glib::Propagation::Proceed
            }
        ));
        view.add_controller(keys);
        // A wheel over the text moves it from under the hover.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = me)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, _| {
                me.dismiss();
                glib::Propagation::Proceed
            }
        ));
        view.add_controller(wheel);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.dismiss()
        ));
        view.add_controller(focus);
        // The caret moved: a click, a jump or an edit, after which the hover is about old text.
        view.buffer().connect_cursor_position_notify(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| me.dismiss()
        ));
        // The tab was switched away from or closed.
        view.connect_unmap(glib::clone!(
            #[weak(rename_to = me)]
            self,
            move |_| {
                me.dismiss();
                me.popover.unparent();
            }
        ));
    }

    /// The pointer moved over the view: off the word shown, the hover goes; wherever it rests, it
    /// is asked about once [`DELAY`] has passed.
    fn moved(self: &Rc<Self>, x: f64, y: f64) {
        // A motion GTK synthesizes where the pointer already was says nothing new.
        if self.pointer.replace(Some((x, y))) == Some((x, y))
            || self.over_popover.contains_pointer()
        {
            return;
        }
        if self.word.get().is_some() && !self.on_word(x, y) {
            self.dismiss();
        }
        if let Some(waiting) = self.settle.take() {
            waiting.remove();
        }
        let me = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DELAY, move || {
            if let Some(me) = me.upgrade() {
                me.settle.take();
                me.settled();
            }
        });
        self.settle.replace(Some(id));
    }

    /// The pointer left the view or the popover: unless it went from one to the word or into
    /// the other, the hover goes. Looked at once the crossing into wherever it went has landed.
    fn left(self: &Rc<Self>) {
        if let Some(waiting) = self.settle.take() {
            waiting.remove();
        }
        let me = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(me) = me.upgrade() else { return };
            let on_word = me.over_view.contains_pointer()
                && me.pointer.get().is_some_and(|(x, y)| me.on_word(x, y));
            if !on_word && !me.over_popover.contains_pointer() {
                me.dismiss();
            }
        });
    }

    /// Whether view point `x`, `y` is on the word asked about or shown.
    fn on_word(&self, x: f64, y: f64) -> bool {
        let (Some(editor), Some((_, word))) = (self.editor.upgrade(), self.word.get()) else {
            return false;
        };
        let (x, y) =
            editor
                .view()
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        word.contains_point(x, y)
    }

    /// The pointer has rested: ask about the word under it, unless it is the one shown.
    fn settled(self: &Rc<Self>) {
        let (Some(editor), Some((x, y))) = (self.editor.upgrade(), self.pointer.get()) else {
            return;
        };
        let view = editor.view();
        if !self.over_view.contains_pointer() {
            return;
        }
        let (x, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let Some((start, end)) = crate::fold::iter_at_location(view, x, y).and_then(word_around)
        else {
            return;
        };
        let rect = view.iter_location(&start).union(&view.iter_location(&end));
        if rect.intersect(&view.visible_rect()).is_none()
            || self.word.get().is_some_and(|(at, _)| at == start.offset())
        {
            return;
        }
        self.dismiss();
        self.word.set(Some((start.offset(), rect)));
        let (at, pos) = (start.offset(), lang::pos_of(&start));
        let me = Rc::downgrade(self);
        let asking = glib::spawn_future_local(async move {
            let Some(markup) = markup_at(editor.clone(), start).await else {
                return;
            };
            // Before it is shown: GTK does not lay a popover on screen out again as it grows.
            let fixes = fixes_at(editor.clone(), pos).await;
            if let Some(me) = me.upgrade() {
                me.fixes.set((!fixes.is_empty()).then_some(at));
                me.fix.set_visible(!fixes.is_empty());
                me.show(editor.view(), &markup);
            }
        });
        self.asking.replace(Some(asking));
    }

    /// Put `markup` over the word asked about.
    fn show(&self, view: &sourceview5::View, markup: &str) {
        let Some((at, word)) = self.word.get() else {
            return;
        };
        let (chars, height) = limits(view, &view.buffer().iter_at_offset(at));
        self.label.set_max_width_chars(chars);
        self.label.set_markup(markup);
        self.label.set_size_request(-1, -1);
        let (_, wide, _, _) = self.label.measure(gtk::Orientation::Horizontal, -1);
        // Held at the width it asks for: the popover's surface takes that width, but a
        // start-aligned popover is laid out at the least width its wrapped text keeps its height
        // in, which left the rest of the surface unpainted.
        self.label.set_size_request(wide, -1);
        // A scrollbar that might show makes a one-line hover two lines tall, so there is one
        // only where the text runs past the bound.
        let (_, tall, _, _) = self.label.measure(gtk::Orientation::Vertical, wide);
        self.scroller.set_max_content_height(height);
        self.scroller
            .set_vscrollbar_policy(match height >= 0 && tall > height {
                true => gtk::PolicyType::Automatic,
                false => gtk::PolicyType::Never,
            });
        self.scroller.vadjustment().set_value(0.0);
        let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, word.x(), word.y());
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(
            x,
            y,
            word.width(),
            word.height(),
        )));
        self.popover.popup();
    }

    /// Take the hover down, and drop whatever was being waited for or asked.
    fn dismiss(&self) {
        if let Some(waiting) = self.settle.take() {
            waiting.remove();
        }
        if let Some(asking) = self.asking.take() {
            asking.abort();
        }
        self.word.set(None);
        self.fixes.set(None);
        if self.popover.is_visible() {
            self.popover.popdown();
        }
    }

    /// Quick Fix… was pressed: the hover goes, and the caret goes to the fixes, where the menu
    /// `Alt+Return` opens comes up.
    fn fix(&self) {
        let (Some(editor), Some(at)) = (self.editor.upgrade(), self.fixes.take()) else {
            return;
        };
        self.dismiss();
        let view = editor.view();
        // A notebook selects the cell whose text takes the keyboard.
        view.grab_focus();
        view.buffer()
            .place_cursor(&view.buffer().iter_at_offset(at));
        let _ = view.activate_action("win.quick-fix", None);
    }
}

/// The run of non-space characters `at` is in, as GtkSourceView's hover takes a word: from the
/// line's start or the last space before it to the line's end or the next space. `None` on a
/// space or past the text.
fn word_around(at: gtk::TextIter) -> Option<(gtk::TextIter, gtk::TextIter)> {
    if at.is_end() || at.char().is_whitespace() {
        return None;
    }
    let mut start = at;
    while !start.starts_line() {
        let mut before = start;
        before.backward_char();
        if before.char().is_whitespace() {
            break;
        }
        start = before;
    }
    let mut end = at;
    while !end.ends_line() && !end.char().is_whitespace() && end.forward_char() {}
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What fits to the window's edge, within `NARROWEST` and `WIDTH` characters, and half the
    /// window's height.
    #[test]
    fn a_hover_is_bounded_by_the_room_the_window_leaves() {
        assert_eq!(bounds((1400, 900), 10.0), (WIDTH, 450));
        assert_eq!(bounds((540, 500), 10.0), (54, 250));
        assert_eq!(bounds((100, 500), 10.0), (NARROWEST, 250));
        assert_eq!(bounds((540, 500), 0.0), (WIDTH, 250), "no metrics yet");
    }

    /// DejaVu Sans Mono at 11 pt is 9.6 px a character: counted as 9, the hover ran 30 px past
    /// the window.
    #[test]
    fn a_fractional_character_width_is_not_rounded_into_more_characters() {
        assert_eq!(bounds((577, 900), 9.6), (60, 450));
    }

    #[test]
    fn inline_markup_becomes_pango_and_the_rest_is_escaped() {
        assert_eq!(
            markup_of("**fn** `add<T>(a: T)` *twice*"),
            "<b>fn</b> <tt>add&lt;T&gt;(a: T)</tt> <i>twice</i>"
        );
    }

    #[test]
    fn a_fenced_block_keeps_its_lines_and_a_heading_is_one_bold_one() {
        assert_eq!(
            markup_of("# Title\n\n```rust\nfn a() {}\nfn b() {}\n```"),
            "<b>Title</b>\n<tt>fn a() {}\nfn b() {}\n</tt>"
        );
    }

    #[test]
    fn a_link_is_its_text_and_paragraphs_keep_one_blank_line() {
        assert_eq!(
            markup_of("See [the docs](https://example.com).\n\nSecond & last."),
            "See the docs.\n\nSecond &amp; last."
        );
    }
}
