//! A cell's stored outputs as widgets, each in the first form core's `pick` says a widget draws
//! (user decision 2026-10-19): a picture, recoloured as an image is; a plain table as a grid;
//! markdown as a note's text; LaTeX typeset as a diagram's formula is, its source until then and
//! for good without WebKit; JSON and text in monospace, ANSI colours from the terminal's palette.
//! What only a script draws says what it was.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use accent_core::notebook::output::{self as core, Form, Item, Output, Table};
use adw::prelude::*;
use gtk::{gdk, glib};

use crate::diagram::math;
use crate::editor::Flavour;
use crate::{highlight, look, theme};

/// What the drill counts of a notebook's outputs.
#[cfg(feature = "bench")]
#[derive(Default, Clone, Copy)]
pub struct Counts {
    pub pictures: usize,
    pub recoloured: usize,
    pub tables: usize,
    pub latex: usize,
    pub typeset: usize,
    pub ansi: usize,
    pub placeholders: usize,
}

/// The parts of the outputs: what a theme change has to touch, and what the drill counts.
enum Part {
    /// A picture's data, decoded and recoloured on a worker each time the look changes.
    Picture {
        picture: gtk::Picture,
        bytes: Rc<Vec<u8>>,
        svg: bool,
        recoloured: Rc<Cell<bool>>,
    },
    Ansi(gtk::TextView),
    Markdown(sourceview5::View, sourceview5::Buffer),
    Latex(Rc<Latex>),
    Table,
    Placeholder,
}

/// A cell's outputs: the box they are drawn in, and a slot per output, so a run redraws only
/// the outputs it changed.
pub struct Outputs {
    pub widget: gtk::Box,
    slots: RefCell<Vec<Slot>>,
}

/// One output's widgets in the box, and its parts.
#[derive(Default)]
struct Slot {
    widgets: Vec<gtk::Widget>,
    parts: Vec<Part>,
    stream: Option<Stream>,
}

struct Stream {
    raw: String,
    stderr: bool,
    view: gtk::TextView,
}

impl Outputs {
    /// The widgets for `outputs`, a code cell's.
    pub fn new(outputs: &[Output]) -> Outputs {
        let shown = Outputs::empty();
        for output in outputs {
            shown.add(output);
        }
        shown
    }

    /// The pictures a markdown cell carries, under it.
    pub fn attachments(attachments: &[core::Attachment]) -> Outputs {
        let shown = Outputs::empty();
        for attachment in attachments {
            if let Some(Form::Picture { mime, data }) = core::pick(&attachment.items) {
                shown.slots.borrow_mut().push(Slot::default());
                shown.picture(mime, data);
            }
        }
        shown
    }

    fn empty() -> Outputs {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        widget.add_css_class("notebook-outputs");
        Outputs {
            widget,
            slots: RefCell::default(),
        }
    }

    /// Show `tail` in place of the outputs from the `from`th on.
    pub fn show(&self, from: usize, tail: &[Output]) {
        if let [Output::Stream { stderr, text }] = tail {
            let mut slots = self.slots.borrow_mut();
            if from + 1 == slots.len()
                && let Some(Stream {
                    raw,
                    stderr: previous,
                    view,
                }) = slots[from].stream.as_mut()
                && previous == stderr
                && let Some(more) = plain_append(raw, text)
            {
                let mut end = view.buffer().end_iter();
                view.buffer().insert(&mut end, more);
                raw.push_str(&text[raw.len()..]);
                return;
            }
        }
        let gone = {
            let mut slots = self.slots.borrow_mut();
            let from = from.min(slots.len());
            slots.split_off(from)
        };
        for widget in gone.into_iter().flat_map(|slot| slot.widgets) {
            self.widget.remove(&widget);
        }
        for output in tail {
            self.add(output);
        }
    }

    /// Put `widget` last, in the last output's slot.
    fn put(&self, widget: &impl IsA<gtk::Widget>) {
        self.widget.append(widget);
        if let Some(slot) = self.slots.borrow_mut().last_mut() {
            slot.widgets.push(widget.clone().upcast());
        }
    }

    fn keep(&self, part: Part) {
        if let Some(slot) = self.slots.borrow_mut().last_mut() {
            slot.parts.push(part);
        }
    }

    /// How many outputs are shown.
    #[cfg(feature = "bench")]
    pub fn shown(&self) -> usize {
        self.slots.borrow().len()
    }

    #[cfg(feature = "bench")]
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for part in self.slots.borrow().iter().flat_map(|slot| &slot.parts) {
            match part {
                Part::Picture { recoloured, .. } => {
                    c.pictures += 1;
                    c.recoloured += usize::from(recoloured.get());
                }
                Part::Latex(latex) => {
                    c.latex += 1;
                    c.typeset += usize::from(latex.typeset.get());
                }
                Part::Ansi(_) => c.ansi += 1,
                Part::Table => c.tables += 1,
                Part::Placeholder => c.placeholders += 1,
                Part::Markdown(..) => {}
            }
        }
        c
    }

    fn add(&self, output: &Output) {
        self.slots.borrow_mut().push(Slot::default());
        match output {
            Output::Stream { stderr, text } => {
                let view = self.text(&core::carriage(text), *stderr);
                if !text.contains(['\r', '\u{1b}']) {
                    self.slots.borrow_mut().last_mut().unwrap().stream = Some(Stream {
                        raw: text.clone(),
                        stderr: *stderr,
                        view,
                    });
                }
            }
            Output::Error {
                name,
                value,
                traceback,
            } => match traceback.is_empty() {
                true => {
                    self.text(&format!("{name}: {value}"), true);
                }
                false => {
                    self.text(&traceback.join("\n"), true);
                }
            },
            Output::Data { items, .. } => match core::pick(items) {
                Some(Form::Picture { mime, data }) => self.picture(mime, data),
                Some(Form::Table(table)) => self.table(&table),
                Some(Form::Markdown(text)) => self.markdown(text),
                Some(Form::Latex(tex)) => self.latex(tex),
                Some(Form::Json(text)) => {
                    self.text(&text, false);
                }
                Some(Form::Text(text)) => {
                    self.text(text, false);
                }
                None => self.placeholder(items),
            },
        }
    }

    /// Monospace text, its escape sequences turned into the terminal's colours; tinted as the
    /// terminal tints nothing, for stderr and a traceback.
    fn text(&self, text: &str, stderr: bool) -> gtk::TextView {
        let (plain, spans) = core::ansi(text);
        let view = output_view(plain.trim_end_matches('\n'));
        if stderr {
            view.add_css_class("notebook-stderr");
        }
        if !spans.is_empty() {
            let buffer = view.buffer();
            let table = buffer.tag_table();
            for n in 0..16 {
                table.add(&gtk::TextTag::new(Some(&format!("ansi-{n}"))));
            }
            let bold = gtk::TextTag::builder()
                .name("ansi-bold")
                .weight(700)
                .build();
            table.add(&bold);
            let char_at = |byte: usize| plain[..byte.min(plain.len())].chars().count() as i32;
            for span in spans {
                let (start, end) = (
                    buffer.iter_at_offset(char_at(span.range.start)),
                    buffer.iter_at_offset(char_at(span.range.end)),
                );
                if let Some(n) = span.colour {
                    buffer.apply_tag_by_name(&format!("ansi-{n}"), &start, &end);
                }
                if span.bold {
                    buffer.apply_tag(&bold, &start, &end);
                }
            }
            paint_ansi(&view);
            self.keep(Part::Ansi(view.clone()));
        }
        self.put(&view);
        view
    }

    fn picture(&self, mime: &str, data: &str) {
        let svg = mime == "image/svg+xml";
        let bytes = match svg {
            true => data.as_bytes().to_vec(),
            false => glib::base64_decode(data),
        };
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .content_fit(gtk::ContentFit::ScaleDown)
            .halign(gtk::Align::Start)
            .build();
        self.put(&picture);
        let part = Part::Picture {
            picture,
            bytes: Rc::new(bytes),
            svg,
            recoloured: Rc::default(),
        };
        show_picture(&part);
        self.keep(part);
    }

    fn table(&self, table: &Table) {
        self.keep(Part::Table);
        let grid = gtk::Grid::builder()
            .column_spacing(18)
            .row_spacing(4)
            .build();
        grid.add_css_class("notebook-table");
        for cell in &table.cells {
            let label = gtk::Label::builder()
                .label(&cell.text)
                .selectable(true)
                .xalign(if cell.header { 0.0 } else { 1.0 })
                .build();
            if cell.header {
                label.add_css_class("heading");
            }
            grid.attach(
                &label,
                cell.col as i32,
                cell.row as i32,
                cell.cols as i32,
                cell.rows as i32,
            );
        }
        let scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&grid)
            .build();
        self.put(&scroller);
        for note in &table.notes {
            let label = gtk::Label::builder()
                .label(note)
                .xalign(0.0)
                .selectable(true)
                .build();
            label.add_css_class("dim-label");
            self.put(&label);
        }
    }

    fn markdown(&self, text: &str) {
        let (view, buffer) = crate::editor::build(Flavour::Note, None, text);
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.add_css_class("notebook-output");
        highlight::apply(&buffer);
        highlight::restyle(&buffer, &view);
        self.put(&view);
        self.keep(Part::Markdown(view, buffer));
    }

    fn latex(&self, latex: &str) {
        let stack = gtk::Stack::new();
        let source = output_view(latex);
        let picture = gtk::Picture::builder()
            .can_shrink(false)
            .halign(gtk::Align::Start)
            .build();
        stack.add_named(&source, Some("source"));
        stack.add_named(&picture, Some("typeset"));
        stack.set_visible_child_name("source");
        self.put(&stack);
        let latex = Rc::new(Latex {
            stack,
            picture,
            tex: latex.to_string(),
            key: Cell::new(None),
            typeset: Cell::new(false),
        });
        typeset(&latex);
        self.keep(Part::Latex(latex));
    }

    /// One line naming what only a script could have shown.
    fn placeholder(&self, items: &[Item]) {
        self.keep(Part::Placeholder);
        let mime = items
            .iter()
            .map(|i| i.mime.as_str())
            .find(|m| *m != "text/plain")
            .unwrap_or("an empty");
        let label = gtk::Label::builder()
            .label(format!("A {mime} output, which only a script can show"))
            .xalign(0.0)
            .wrap(true)
            .build();
        label.add_css_class("dim-label");
        self.put(&label);
    }

    /// A new theme, or the Images switch turned: the pictures recoloured again, the colours of
    /// the text and the formulas' ink.
    pub fn restyle(&self) {
        for part in self.slots.borrow().iter().flat_map(|slot| &slot.parts) {
            match part {
                Part::Picture { .. } => show_picture(part),
                Part::Ansi(view) => paint_ansi(view),
                Part::Markdown(view, buffer) => {
                    crate::editor::sync_scheme(buffer);
                    highlight::restyle(buffer, view);
                }
                Part::Latex(latex) => typeset(latex),
                Part::Table | Part::Placeholder => {}
            }
        }
    }
}

/// The visible suffix of an append-only stream without terminal controls.
fn plain_append<'a>(old: &str, new: &'a str) -> Option<&'a str> {
    let added = new.strip_prefix(old)?;
    if old.contains(['\r', '\u{1b}']) || added.contains(['\r', '\u{1b}']) {
        return None;
    }
    new.trim_end_matches('\n')
        .strip_prefix(old.trim_end_matches('\n'))
}

/// A read-only monospace view of `text`, selectable.
fn output_view(text: &str) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    view.add_css_class("notebook-output");
    view.buffer().set_text(text);
    view
}

/// The terminal's sixteen colours on `view`'s tags, for the theme in force.
fn paint_ansi(view: &gtk::TextView) {
    let palette = theme::terminal_palette(adw::StyleManager::default().is_dark());
    let table = view.buffer().tag_table();
    for (n, hex) in palette.iter().enumerate() {
        if let (Some(tag), Ok(colour)) =
            (table.lookup(&format!("ansi-{n}")), gdk::RGBA::parse(*hex))
        {
            tag.set_foreground_rgba(Some(&colour));
        }
    }
}

/// How wide a picture is shown at most, in pixels.
const MAX_WIDTH: i32 = 720;

/// Decode and recolour a picture on a worker, then show it; a picture that is not one stays
/// empty.
fn show_picture(part: &Part) {
    let Part::Picture {
        picture,
        bytes,
        svg,
        recoloured,
    } = part
    else {
        return;
    };
    let (bytes, svg, look) = (bytes.clone(), *svg, look::Look::now());
    let scale = picture.scale_factor().max(1);
    let data = bytes.to_vec();
    let (picture, recoloured) = (picture.downgrade(), recoloured.clone());
    glib::spawn_future_local(async move {
        let shown = crate::work::off_thread("notebook picture", move || {
            look::show_bytes(&data, svg, look, scale).ok()
        })
        .await
        .flatten();
        let (Some(picture), Some((shown, changed))) = (picture.upgrade(), shown) else {
            return;
        };
        let paintable = shown.paintable();
        // At its own size, as Jupyter shows a picture, but no wider than a column of text: a
        // picture's natural size alone is not what a shrinkable one is given in a column.
        let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
        let w_shown = w.min(MAX_WIDTH);
        let h_shown = match w {
            0 => h,
            w => (f64::from(h) * f64::from(w_shown) / f64::from(w)).round() as i32,
        };
        picture.set_size_request(w_shown, h_shown);
        picture.set_paintable(Some(&paintable));
        recoloured.set(changed);
    });
}

/// A formula output, shown as its source until it is typeset.
struct Latex {
    stack: gtk::Stack,
    picture: gtk::Picture,
    tex: String,
    /// What it is typeset under now: its MathML in the ink of the theme it was asked in.
    key: Cell<Option<u64>>,
    typeset: Cell<bool>,
}

/// Ask the shared typesetter for `latex` in the theme's ink, and show it once it comes in. Its
/// source stays where WebKit is missing or the formula does not parse.
fn typeset(latex: &Rc<Latex>) {
    let (tex, display) = tex_of(&latex.tex);
    let (Some(typesetter), Some(mathml)) =
        (math::shared(), accent_core::markdown::mathml(tex, display))
    else {
        return;
    };
    let dark = adw::StyleManager::default().is_dark();
    let ink = gdk::RGBA::parse(theme::view_fg(dark)).map_or([0, 0, 0], theme::rgb_of);
    let html = format!(
        "<div style=\"font-size: 16px; color: rgb({}, {}, {});\">{mathml}</div>",
        ink[0], ink[1], ink[2]
    );
    let key = math::key_of(&html);
    latex.key.set(Some(key));
    let land = move |latex: &Latex, typesetter: &math::Typesetter| match typesetter.get(key) {
        Some(Some(rendered)) => {
            latex.picture.set_paintable(Some(&rendered.texture));
            let (w, h) = rendered.size;
            latex
                .picture
                .set_size_request(w.ceil() as i32, h.ceil() as i32);
            latex.stack.set_visible_child_name("typeset");
            latex.typeset.set(true);
            true
        }
        Some(None) => true,
        None => false,
    };
    if land(latex, &typesetter) {
        return;
    }
    let weak: Weak<Latex> = Rc::downgrade(latex);
    let shared = Rc::downgrade(&typesetter);
    typesetter.connect_ready(move || {
        let (Some(latex), Some(typesetter)) = (weak.upgrade(), shared.upgrade()) else {
            return false;
        };
        // A newer look asked again under another key; this one's answer is not wanted.
        latex.key.get() == Some(key) && !land(&latex, &typesetter)
    });
    typesetter.ask(math::Label { key, html });
}

/// The TeX inside a `text/latex` output's delimiters, and whether it is set as a display.
fn tex_of(latex: &str) -> (&str, bool) {
    let t = latex.trim();
    for (open, close, display) in [
        ("$$", "$$", true),
        ("\\[", "\\]", true),
        ("$", "$", false),
        ("\\(", "\\)", false),
    ] {
        if t.len() >= open.len() + close.len()
            && let Some(inner) = t.strip_prefix(open).and_then(|t| t.strip_suffix(close))
        {
            return (inner.trim(), display);
        }
    }
    (t, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_stream_append_keeps_the_visible_suffix() {
        assert_eq!(plain_append("first\n", "first\nsecond\n"), Some("\nsecond"));
        assert_eq!(plain_append("first\n", "first\n\n"), Some(""));
        assert_eq!(plain_append("first", "other"), None);
        assert_eq!(plain_append("first", "first\rnext"), None);
        assert_eq!(plain_append("first", "first\u{1b}[31mnext"), None);
    }

    #[test]
    fn a_formula_is_read_inside_its_delimiters() {
        assert_eq!(
            tex_of("$\\displaystyle \\int x^{2}\\, dx$"),
            ("\\displaystyle \\int x^{2}\\, dx", false)
        );
        assert_eq!(tex_of("$$x$$"), ("x", true));
        assert_eq!(
            tex_of("\\begin{equation}x\\end{equation}"),
            ("\\begin{equation}x\\end{equation}", true)
        );
    }
}
