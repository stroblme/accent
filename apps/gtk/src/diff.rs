//! Side-by-side line diff, used by the sync-conflict resolver.
//!
//! Deliberately knows nothing about vaults or conflicts: it takes two texts and a precomputed
//! `DiffLine` list, so Phase 4 can point it at a git diff without touching this file.

use accent_core::diff::{DiffLine, Op};
use adw::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

const TAG_ADDED: &str = "added";
const TAG_REMOVED: &str = "removed";
const TAG_FILLER: &str = "filler";

/// Row backgrounds. This is the one place DESIGN.md's "only accent, foreground and is_dark" rule
/// bends: a diff has to read as green and red, and libadwaita publishes its success/error colours
/// as CSS variables only, which GTK will resolve in a stylesheet but not hand back to Rust. So the
/// hue is a fixed weight and everything else is derived: it is mixed with the theme foreground,
/// which pulls the tint dark on a light theme and light on a dark one, then laid down at a low
/// alpha over the view background so the text on top keeps its contrast either way.
const ADDED_HUE: (f32, f32, f32) = (0.15, 0.70, 0.35);
const REMOVED_HUE: (f32, f32, f32) = (0.80, 0.20, 0.25);
/// Share of the tint that is the hue; the rest is the foreground.
const HUE_MIX: f32 = 0.65;
const CHANGE_ALPHA: f32 = 0.16;
/// A filler row has no content, so it whispers instead of shouting. Matches the code-block
/// background in `highlight.rs` (0.07), which is the same "this area is inert" signal.
const FILLER_ALPHA: f32 = 0.06;

/// Which of the two texts a pane shows. Decides the line number and the tag of every row.
#[derive(Clone, Copy)]
enum Side {
    Old,
    New,
}

impl Side {
    fn number(self, line: &DiffLine) -> Option<usize> {
        match self {
            Side::Old => line.old_line,
            Side::New => line.new_line,
        }
    }

    /// The tag a present row gets, or `None` for an unchanged one.
    fn tag(self, line: &DiffLine) -> Option<&'static str> {
        match (self, line.op) {
            (Side::Old, Op::Delete) => Some(TAG_REMOVED),
            (Side::New, Op::Insert) => Some(TAG_ADDED),
            // `align` never puts an insertion on the old side, nor a deletion on the new one.
            _ => None,
        }
    }
}

/// Turn the flat diff into two equally long columns: an `Equal` line sits on both sides, a run of
/// `Delete`s is paired row by row with the `Insert` run beside it, and whichever run is shorter
/// gets `None` fillers so the columns stay in step.
///
/// This is the whole correctness surface of the widget, so it stays a plain function over plain
/// data and is unit-tested below.
fn align(lines: &[DiffLine]) -> (Vec<Option<&DiffLine>>, Vec<Option<&DiffLine>>) {
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut dels, mut ins): (Vec<&DiffLine>, Vec<&DiffLine>) = (Vec::new(), Vec::new());
    for line in lines {
        match line.op {
            Op::Equal => {
                flush(&mut dels, &mut ins, &mut left, &mut right);
                left.push(Some(line));
                right.push(Some(line));
            }
            // A delete after an insert starts a new pairing: `similar` emits deletes before
            // inserts within a hunk, so this only guards against input that does not.
            Op::Delete => {
                if !ins.is_empty() {
                    flush(&mut dels, &mut ins, &mut left, &mut right);
                }
                dels.push(line);
            }
            Op::Insert => ins.push(line),
        }
    }
    flush(&mut dels, &mut ins, &mut left, &mut right);
    (left, right)
}

fn flush<'a>(
    dels: &mut Vec<&'a DiffLine>,
    ins: &mut Vec<&'a DiffLine>,
    left: &mut Vec<Option<&'a DiffLine>>,
    right: &mut Vec<Option<&'a DiffLine>>,
) {
    for i in 0..dels.len().max(ins.len()) {
        left.push(dels.get(i).copied());
        right.push(ins.get(i).copied());
    }
    dels.clear();
    ins.clear();
}

fn column_text(rows: &[Option<&DiffLine>]) -> String {
    rows.iter()
        .map(|r| r.map_or("", |l| l.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn numbers(rows: &[Option<&DiffLine>], side: Side) -> Vec<Option<usize>> {
    rows.iter()
        .map(|r| r.and_then(|l| side.number(l)))
        .collect()
}

fn install_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for name in [TAG_ADDED, TAG_REMOVED, TAG_FILLER] {
        table.add(&gtk::TextTag::new(Some(name)));
    }
}

fn tint(hue: (f32, f32, f32), fg: gdk::RGBA, alpha: f32) -> gdk::RGBA {
    let mix = |h: f32, f: f32| h * HUE_MIX + f * (1.0 - HUE_MIX);
    gdk::RGBA::new(
        mix(hue.0, fg.red()),
        mix(hue.1, fg.green()),
        mix(hue.2, fg.blue()),
        alpha,
    )
}

/// Re-derive the row backgrounds from the resolved theme foreground. Call once the view is mapped
/// and again on every `notify::dark`, exactly as `highlight::restyle` does for the editor.
fn restyle(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let fg = view.color();
    let table = buffer.tag_table();
    let set = |name: &str, colour: gdk::RGBA| {
        if let Some(t) = table.lookup(name) {
            t.set_paragraph_background_rgba(Some(&colour));
        }
    };
    set(TAG_ADDED, tint(ADDED_HUE, fg, CHANGE_ALPHA));
    set(TAG_REMOVED, tint(REMOVED_HUE, fg, CHANGE_ALPHA));
    set(
        TAG_FILLER,
        gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), FILLER_ALPHA),
    );
}

/// Print the *source* line number of each row.
///
/// GtkSourceView's own gutter numbers buffer rows, which is wrong here: a filler row has no line
/// in either text, and once the columns diverge a row's number stops following its index. Every
/// label is padded to the same character width, and the renderer starts out holding a blank of
/// that width, so it measures the same on every line and the gutter cannot jitter while scrolling.
fn install_line_numbers(view: &sourceview5::View, numbers: Vec<Option<usize>>) {
    let width = numbers
        .iter()
        .flatten()
        .max()
        .copied()
        .unwrap_or(1)
        .to_string()
        .len();
    let blank = " ".repeat(width);
    let renderer = sourceview5::GutterRendererText::new();
    renderer.set_xalign(1.0);
    renderer.set_xpad(6);
    renderer.set_text(&blank);
    renderer.connect_query_data(move |r, _, line| {
        match numbers.get(line as usize).copied().flatten() {
            Some(n) => r.set_text(&format!("{n:>width$}")),
            None => r.set_text(&blank),
        }
    });
    // Disambiguated: `TextViewExt` has a `gutter` of its own.
    sourceview5::prelude::ViewExt::gutter(view, gtk::TextWindowType::Left).insert(&renderer, 0);
}

struct Pane {
    root: gtk::Box,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    scroller: gtk::ScrolledWindow,
}

fn pane(title: &str, rows: &[Option<&DiffLine>], side: Side) -> Pane {
    let buffer = sourceview5::Buffer::new(None);
    install_tags(&buffer);
    buffer.set_text(&column_text(rows));
    for (i, row) in rows.iter().enumerate() {
        let name = match row {
            None => TAG_FILLER,
            Some(line) => match side.tag(line) {
                Some(name) => name,
                None => continue,
            },
        };
        // The range runs to the start of the next row so it covers the newline as well: a
        // paragraph background needs the paragraph tagged, and a filler row is empty.
        let start = buffer
            .iter_at_line(i as i32)
            .unwrap_or_else(|| buffer.end_iter());
        let end = buffer
            .iter_at_line(i as i32 + 1)
            .unwrap_or_else(|| buffer.end_iter());
        buffer.apply_tag_by_name(name, &start, &end);
    }

    let view = sourceview5::View::new();
    view.set_buffer(Some(&buffer));
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    // Off on purpose: `install_line_numbers` prints the source numbers instead.
    view.set_show_line_numbers(false);
    view.set_wrap_mode(gtk::WrapMode::None);
    install_line_numbers(&view, numbers(rows, side));

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();

    let header = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    header.add_css_class("heading");
    header.add_css_class("dim-label");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&scroller);
    Pane {
        root,
        view,
        buffer,
        scroller,
    }
}

/// A two-pane diff. `left` and `right` are (title, text) pairs; `lines` is the precomputed diff.
///
/// ponytail: line granularity only. A one-character edit paints the whole row, which is what a
/// conflict resolver needs to decide between two files. Word-level highlighting inside a changed
/// pair is the upgrade (`similar::TextDiff::iter_inline_changes` over the paired rows), and it
/// wants a second tag pair and per-row char offsets; add it when line rows prove too coarse.
pub fn view(left: (&str, &str), right: (&str, &str), lines: &[DiffLine]) -> gtk::Widget {
    // The texts are in the signature so a caller that already holds them can hand them over; the
    // panes are built from `lines`, which carries every line of both sides already.
    let (left_rows, right_rows) = align(lines);
    let old = pane(left.0, &left_rows, Side::Old);
    let new = pane(right.0, &right_rows, Side::New);
    // One adjustment per direction: two views of the same rows must not be able to drift apart.
    new.scroller
        .set_vadjustment(Some(&old.scroller.vadjustment()));
    new.scroller
        .set_hadjustment(Some(&old.scroller.hadjustment()));

    // Weak, both because this closure is connected to one of the very views it restyles and
    // because the style manager below outlives the dialog: a strong capture either way is a cycle
    // that keeps two buffers alive for the life of the process.
    let (ob, ov, nb, nv) = (
        old.buffer.clone(),
        old.view.clone(),
        new.buffer.clone(),
        new.view.clone(),
    );
    let restyle_both = Rc::new(glib::clone!(
        #[weak]
        ob,
        #[weak]
        ov,
        #[weak]
        nb,
        #[weak]
        nv,
        move || {
            restyle(&ob, &ov);
            restyle(&nb, &nv);
        }
    ));
    // `view.color()` only resolves the theme foreground once the widget is mapped.
    old.view.connect_map({
        let restyle_both = restyle_both.clone();
        move |_| restyle_both()
    });
    let style = adw::StyleManager::default();
    let dark_handler = style.connect_dark_notify(move |_| restyle_both());

    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    paned.set_start_child(Some(&old.root));
    paned.set_end_child(Some(&new.root));
    paned.set_resize_start_child(true);
    paned.set_shrink_start_child(false);
    paned.set_resize_end_child(true);
    paned.set_shrink_end_child(false);
    // Even split. The widget cannot know how wide its host will be, so the position is set once
    // from an idle, which runs after the first layout pass has given the paned a real width.
    paned.connect_map(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || {
            if p.width() > 0 {
                p.set_position(p.width() / 2);
            }
        });
    });
    // The style manager is a process-wide singleton, so a handler left on it would outlive this
    // widget and keep both buffers alive with it.
    let dark_handler = Cell::new(Some(dark_handler));
    paned.connect_destroy(move |_| {
        if let Some(id) = dark_handler.take() {
            style.disconnect(id);
        }
    });
    paned.upcast()
}

/// What the user decided about a sync conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    KeepMine,
    KeepTheirs,
}

/// The conflict resolver built on top of [`view`]. `on_choice` receives the user's decision; the
/// caller does the file work, because this module does not touch the vault.
///
/// `title` names the kind of conflict (a sync copy, a file that changed under an open buffer);
/// `original` and `conflict` are (label, text), the label being what each pane is called. Escape
/// closes without a choice, which is a real option here: neither side is touched until one is
/// picked.
pub fn present_conflict(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    original: (&str, &str),
    conflict: (&str, &str),
    on_choice: impl Fn(Choice) + 'static,
) {
    let lines = accent_core::diff::lines(original.1, conflict.1);
    let mine = format!("Mine — {}", original.0);
    let theirs = format!("Theirs — {}", conflict.0);
    let diff = view((&mine, original.1), (&theirs, conflict.1), &lines);

    let header = adw::HeaderBar::new();
    let note = original.0.rsplit('/').next().unwrap_or(original.0);
    header.set_title_widget(Some(&adw::WindowTitle::new(title, note)));

    let keep_theirs = gtk::Button::with_label("Keep Theirs");
    let keep_mine = gtk::Button::with_label("Keep Mine");
    keep_mine.add_css_class("suggested-action");
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::End)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    buttons.append(&keep_theirs);
    buttons.append(&keep_mine);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&diff));
    toolbar.add_bottom_bar(&buttons);

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(1100)
        .content_height(700)
        .child(&toolbar)
        .build();

    let on_choice = Rc::new(on_choice);
    for (button, choice) in [
        (&keep_theirs, Choice::KeepTheirs),
        (&keep_mine, Choice::KeepMine),
    ] {
        // Weak dialog: the button is inside it, so a strong capture here is the cycle that kept
        // every resolved conflict, both texts included, alive until the process exited.
        let (dialog, on_choice) = (dialog.downgrade(), on_choice.clone());
        button.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            on_choice(choice);
        });
    }
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_core::diff::lines;

    fn texts<'a>(rows: &[Option<&'a DiffLine>]) -> Vec<Option<&'a str>> {
        rows.iter().map(|r| r.map(|l| l.text.as_str())).collect()
    }

    #[test]
    fn alignment_pairs_equal_lines_and_pads_changes() {
        let d = lines("alpha\nbravo\ncharlie\n", "alpha\nbravo two\ncharlie\n");
        let (left, right) = align(&d);
        assert_eq!(left.len(), 3, "one row per line, the change paired up");
        assert_eq!(right.len(), 3);
        assert_eq!(
            texts(&left),
            vec![Some("alpha"), Some("bravo"), Some("charlie")]
        );
        assert_eq!(
            texts(&right),
            vec![Some("alpha"), Some("bravo two"), Some("charlie")]
        );
    }

    #[test]
    fn alignment_handles_pure_insert_and_pure_delete() {
        let d = lines("alpha\n", "alpha\nbravo\ncharlie\n");
        let (left, right) = align(&d);
        assert_eq!(left.len(), right.len());
        assert_eq!(texts(&left), vec![Some("alpha"), None, None]);
        assert_eq!(
            texts(&right),
            vec![Some("alpha"), Some("bravo"), Some("charlie")]
        );

        let d = lines("alpha\nbravo\ncharlie\n", "alpha\n");
        let (left, right) = align(&d);
        assert_eq!(left.len(), right.len());
        assert_eq!(
            texts(&left),
            vec![Some("alpha"), Some("bravo"), Some("charlie")]
        );
        assert_eq!(texts(&right), vec![Some("alpha"), None, None]);
    }

    #[test]
    fn alignment_keeps_source_line_numbers() {
        let d = lines("alpha\nbravo\ncharlie\n", "alpha\nx\ny\nbravo\ncharlie\n");
        let (left, right) = align(&d);
        assert_eq!(
            numbers(&left, Side::Old),
            vec![Some(1), None, None, Some(2), Some(3)],
            "filler rows carry no number and the rest keep their old-text line"
        );
        assert_eq!(
            numbers(&right, Side::New),
            vec![Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
    }
}
