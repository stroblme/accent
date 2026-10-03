//! What a tab does to whole lines: the clipboard's line cut and copy, Insert Line Below,
//! duplicate and delete, the comment toggle, wrapping, and the template snippets whose Tab stops
//! are walked through the text they inserted. The plain-text middle-click paste sits beside the
//! cut and copy. The line commands and the line cut and copy act at every caret of a column.

use super::{Tab, caret, line_end};
use crate::{comment, multicaret};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use sourceview5::prelude::*;

/// The spaces and tabs a line opens with, which is what a line inserted below it copies.
fn leading_indent(line: &str) -> &str {
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// The last line Duplicate Line copies, from the lines the selection starts and ends on and the
/// column it ends in. Every line it touches, except that a selection ending at the very start of a
/// line leaves that line out: VS Code's rule, so lines selected whole with Shift+Down are copied
/// without the one below them.
fn last_copied(first: i32, last: i32, end_column: i32) -> i32 {
    match last > first && end_column == 0 {
        true => last - 1,
        false => last,
    }
}

/// A line as the clipboard should carry it: with the newline back that a last line does not have
/// of its own, so pasting it opens a line rather than splicing into the one under the caret.
fn paste_ready(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("{line}\n"),
    }
}

/// Line `line`, from its start to the start of the next one, so the trailing newline is part of
/// it except on a last line that has none.
fn line_bounds(buffer: &gtk::TextBuffer, line: i32) -> (gtk::TextIter, gtk::TextIter) {
    let start = buffer
        .iter_at_line(line)
        .unwrap_or_else(|| buffer.end_iter());
    let mut end = start;
    // On the last line this lands on the end of the buffer and reports failure, which is
    // exactly where the line ends, so the answer is the same either way.
    end.forward_line();
    (start, end)
}

/// The column of carets `view` holds, if it holds one.
fn column(view: &sourceview5::View) -> Option<&multicaret::View> {
    view.downcast_ref::<multicaret::View>()
        .filter(|view| view.has_carets())
}

/// Line ranges, first and last, sorted and made one where they share a line, so that no line is
/// taken twice. Ranges that only meet stay apart: carets on neighbouring lines are two lines.
fn runs(mut ranges: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
    ranges.sort_unstable();
    let mut runs: Vec<(i32, i32)> = Vec::new();
    for (first, last) in ranges {
        match runs.last_mut() {
            Some(run) if first <= run.1 => run.1 = run.1.max(last),
            _ => runs.push((first, last)),
        }
    }
    runs
}

/// The runs of lines a column's carets cover: each caret's selection by [`last_copied`]'s rule,
/// the caret's own line where it has none.
fn covered(column: &multicaret::View) -> Vec<(i32, i32)> {
    let ranges = column
        .selections()
        .iter()
        .map(|(start, end)| {
            let first = start.line();
            (first, last_copied(first, end.line(), end.line_offset()))
        })
        .collect();
    runs(ranges)
}

/// The lines one caret's selection covers, first and last, by [`last_copied`]'s rule: what the
/// line commands take with no column of carets up. With nothing selected that is the caret's own
/// line.
fn selected_lines(buffer: &gtk::TextBuffer) -> (i32, i32) {
    let (from, end) = buffer.selection_bounds().unwrap_or_else(|| {
        let at = caret(buffer);
        (at, at)
    });
    (
        from.line(),
        last_copied(from.line(), end.line(), end.line_offset()),
    )
}

/// Whether any caret of `column` has something selected, which decides whether a cut or copy
/// there takes the selections or whole lines.
fn any_selected(column: &multicaret::View) -> bool {
    column.selections().iter().any(|(start, end)| start != end)
}

/// What a cut or copy at a column takes, VS Code's two cases: each caret's selection, joined by
/// newlines top to bottom, a caret with nothing selected giving an empty line; or, where no caret
/// has anything selected, every caret's whole line.
fn column_text(view: &sourceview5::View, column: &multicaret::View) -> String {
    if !any_selected(column) {
        return whole_lines(view);
    }
    let buffer = view.buffer();
    let texts: Vec<String> = column
        .selections()
        .iter()
        .map(|(start, end)| buffer.text(start, end, true).to_string())
        .collect();
    texts.join("\n")
}

/// What a cut or copy with nothing selected takes: the caret's whole line, or every caret's in a
/// column, each once and top to bottom, as VS Code copies them.
fn whole_lines(view: &sourceview5::View) -> String {
    let buffer = view.buffer();
    let lines = match column(view) {
        Some(column) => column.caret_lines(),
        None => vec![caret(&buffer).line()],
    };
    lines
        .into_iter()
        .map(|line| {
            let (start, end) = line_bounds(&buffer, line);
            paste_ready(&buffer.text(&start, &end, true))
        })
        .collect()
}

/// Delete lines `first` to `last` whole, the last one's newline with them. A last line with no
/// newline of its own takes the one separating it from the line above, or deleting it would leave
/// the blank line it used to sit on.
fn delete_lines(buffer: &gtk::TextBuffer, first: i32, last: i32) {
    let (mut start, _) = line_bounds(buffer, first);
    let (_, mut end) = line_bounds(buffer, last);
    if !buffer.text(&start, &end, true).ends_with('\n') {
        start.backward_char();
    }
    buffer.delete(&mut start, &mut end);
}

/// Repeat lines `first` to `last` below themselves.
///
/// The copy is inserted *above* the lines, at the start of the first one, and that is what moves
/// the carets and the selection down onto it: their marks have right gravity, so text put in front
/// of them carries them along onto the lower of the two blocks. It also means a last line with no
/// newline of its own needs no special case.
fn copy_down(buffer: &gtk::TextBuffer, first: i32, last: i32) {
    let Some(mut from) = buffer.iter_at_line(first) else {
        return;
    };
    let lines = buffer.text(&from, &line_end(buffer, last), true);
    buffer.insert(&mut from, &format!("{lines}\n"));
}

/// Open a line under `line` with its indent, and answer where the caret goes: at the end of it.
fn open_below(buffer: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    // Before the line's own newline, or at the end of the buffer on a last line that has none.
    let mut at = line_end(buffer, line);
    let mut start = at;
    start.set_line_offset(0);
    let indent = leading_indent(&buffer.text(&start, &at, true)).to_string();
    buffer.insert(&mut at, &format!("\n{indent}"));
    at
}

/// VS Code's Copy Line Down: the lines the selection touches ([`last_copied`]) are repeated below
/// themselves, and the caret and the selection move down onto the copy, in the same columns. At a
/// column, every run of lines the carets cover ([`covered`]), each onto its copy.
pub(crate) fn duplicate_line(view: &sourceview5::View) {
    if !view.is_editable() {
        return;
    }
    if let Some(column) = column(view) {
        return column.each_block(&covered(column), |buffer, first, last| {
            copy_down(buffer, first, last);
            None
        });
    }
    let buffer = view.buffer();
    let (first, last) = selected_lines(&buffer);
    buffer.begin_user_action();
    copy_down(&buffer, first, last);
    buffer.end_user_action();
    view.scroll_mark_onscreen(&buffer.get_insert());
}

/// Delete every line the selection covers ([`selected_lines`]), or at a column every line the
/// carets cover, each once. VS Code's rule, and Duplicate Line's: the selected lines in both
/// cases, the caret's own where there is no selection.
pub(crate) fn delete_line(view: &sourceview5::View) {
    if !view.is_editable() {
        return;
    }
    if let Some(column) = column(view) {
        return column.each_block(&covered(column), |buffer, first, last| {
            delete_lines(buffer, first, last);
            None
        });
    }
    let buffer = view.buffer();
    let (first, last) = selected_lines(&buffer);
    buffer.begin_user_action();
    delete_lines(&buffer, first, last);
    buffer.end_user_action();
}

/// VS Code's Insert Line Below: open a line under the caret's and put the caret on it, at the
/// same indent, so a list item or an indented block carries on where it was. That is the idiom
/// `typing.rs` already uses on Return; continuing the marker itself is Return's job, not this
/// one's, because this is also how a line is opened *out* of a list. At a column, a line under
/// each run of lines the carets cover, with every caret of the run moved onto it.
pub(crate) fn newline_below(view: &sourceview5::View) {
    if !view.is_editable() {
        return;
    }
    if let Some(column) = column(view) {
        return column.each_block(&covered(column), |buffer, _, last| {
            Some(open_below(buffer, last))
        });
    }
    let buffer = view.buffer();
    // One user action, so one Ctrl+Z takes the whole line back.
    buffer.begin_user_action();
    let at = open_below(&buffer, caret(&buffer).line());
    buffer.end_user_action();
    buffer.place_cursor(&at);
    view.scroll_mark_onscreen(&buffer.get_insert());
}

/// `text` with the language's comment markers put on or taken off, or `None` where the language
/// names none. GtkSourceView carries the markers in its metadata but does no toggling of its own,
/// so the text goes out to [`crate::comment`] and comes back as one replacement. Line comments go
/// on where `on` says, a column having decided over all its lines, or where `text` is not all
/// commented already.
fn toggled(text: &str, language: &sourceview5::Language, on: Option<bool>) -> Option<String> {
    if let Some(marker) = language.metadata("line-comment-start") {
        let on = on.unwrap_or_else(|| !comment::commented(text, &marker));
        return Some(comment::comment_lines(text, &marker, on));
    }
    let (open, close) = (
        language.metadata("block-comment-start")?,
        language.metadata("block-comment-end")?,
    );
    Some(comment::toggle_block(text, &open, &close))
}

/// Comment or uncomment lines `first` to `last` whole — a marker goes in front of a line, never
/// in front of a word — replacing only the characters that actually change, so a caret in those
/// lines keeps its column instead of riding to the end of a wholesale replacement. `on` as for
/// [`toggled`].
fn toggle_run(
    buffer: &gtk::TextBuffer,
    first: i32,
    last: i32,
    language: &sourceview5::Language,
    on: Option<bool>,
) {
    let Some(start) = buffer.iter_at_line(first) else {
        return;
    };
    let end = line_end(buffer, last);
    let text = buffer.text(&start, &end, true);
    let Some(toggled) = toggled(&text, language, on) else {
        return;
    };
    if toggled != text {
        splice(buffer, start, end, &text, &toggled);
    }
}

/// How many characters `old` and `new` share at the front and at the back without the two runs
/// overlapping. What is left between them is all a replacement has to touch.
fn shared(old: &str, new: &str) -> (usize, usize) {
    let head = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let rest = |text: &str| text.chars().count() - head;
    let tail = old
        .chars()
        .rev()
        .zip(new.chars().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(rest(old))
        .min(rest(new));
    (head, tail)
}

/// Replace `old`, which is what stands between `start` and `end`, with `new`, touching only the
/// run of characters where the two differ ([`shared`]): the head and tail they share are left
/// where they are, and so is every mark inside them.
fn splice(
    buffer: &gtk::TextBuffer,
    mut start: gtk::TextIter,
    mut end: gtk::TextIter,
    old: &str,
    new: &str,
) {
    let (head, tail) = shared(old, new);
    let middle: String = new
        .chars()
        .skip(head)
        .take(new.chars().count() - head - tail)
        .collect();
    start.forward_chars(head as i32);
    end.backward_chars(tail as i32);
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &middle);
}

/// Comment or uncomment the lines the selection covers with the language's own markers, inside a
/// single user action so one Ctrl+Z undoes the whole thing. At a column, every run of lines the
/// carets cover ([`covered`]), the column staying up; whether line comments go on or come off is
/// decided once over all of them, as VS Code does, so a column half commented is commented whole.
pub(crate) fn toggle_comment(view: &sourceview5::View) {
    if !view.is_editable() {
        return;
    }
    let buffer = view.buffer();
    let Some(language) = buffer
        .downcast_ref::<sourceview5::Buffer>()
        .and_then(|buffer| buffer.language())
    else {
        return;
    };
    if let Some(column) = column(view) {
        let runs = covered(column);
        let on = language.metadata("line-comment-start").map(|marker| {
            let text: Vec<String> = runs
                .iter()
                .filter_map(|&(first, last)| {
                    let start = buffer.iter_at_line(first)?;
                    Some(
                        buffer
                            .text(&start, &line_end(&buffer, last), true)
                            .to_string(),
                    )
                })
                .collect();
            !comment::commented(&text.join("\n"), &marker)
        });
        return column.each_block(&runs, |buffer, first, last| {
            toggle_run(buffer, first, last, &language, on);
            None
        });
    }
    let had_selection = buffer.has_selection();
    let (first, last) = selected_lines(&buffer);
    buffer.begin_user_action();
    toggle_run(&buffer, first, last, &language, None);
    buffer.end_user_action();
    // The marker is not part of what was selected, so the lines are selected afresh rather than
    // left to the marks: toggling never adds or removes a line, so they are still these two.
    if had_selection && let Some(start) = buffer.iter_at_line(first) {
        buffer.select_range(&start, &line_end(&buffer, last));
    }
}

/// Cut and copy, always as plain text, and VS Code's whole-line cut and copy: with nothing
/// selected, `Ctrl+X` and `Ctrl+C` take the caret's whole line, its newline with it, so a later
/// paste puts a line back instead of a fragment. At a column they take every caret's selection,
/// or every caret's line where none has one ([`column_text`]), and the cut deletes what it took as
/// one undo step, the column staying up.
///
/// No key handling, and no accelerator either — DESIGN.md's never-bind list keeps `Ctrl+X`/`C`
/// for the widget. Both chords and the context menu emit these two signals, and each handler runs
/// before the inherited one. With a selection it stops that one, which would put a `GtkTextBuffer`
/// on the clipboard: pasting that back applies the copy's tags after the highlighter has re-tagged
/// the text, so a heading's or a bold's stayed on the paste's last run. Without a selection the
/// inherited one does nothing at all; selecting the line and letting it have the line instead
/// would work for the cut and leave the copy selected.
///
/// After a whole-line cut the caret is where the deletion left it, at the start of the following
/// line; VS Code lands on the same line but keeps the column.
pub(crate) fn line_clipboard(view: &sourceview5::View) {
    view.connect_copy_clipboard(|view| {
        let buffer = view.buffer();
        // Stopped here too, or the inherited one copies the primary's selection over it.
        if let Some(column) = column(view) {
            view.clipboard().set_text(&column_text(view, column));
            view.stop_signal_emission_by_name("copy-clipboard");
            return;
        }
        if let Some((start, end)) = buffer.selection_bounds() {
            view.clipboard().set_text(&buffer.text(&start, &end, true));
            view.stop_signal_emission_by_name("copy-clipboard");
            return;
        }
        view.clipboard().set_text(&whole_lines(view));
    });
    view.connect_cut_clipboard(|view| {
        let buffer = view.buffer();
        if let Some(column) = column(view) {
            view.clipboard().set_text(&column_text(view, column));
            view.stop_signal_emission_by_name("cut-clipboard");
            // A cut on a read-only view is a copy, as GTK's own is.
            if view.is_editable() {
                match any_selected(column) {
                    true => column.delete_selections(),
                    false => delete_line(view),
                }
            }
            return;
        }
        if let Some((start, end)) = buffer.selection_bounds() {
            view.clipboard().set_text(&buffer.text(&start, &end, true));
            // Refused on a read-only view, where a cut is a copy: what GTK's own cut does.
            buffer.delete_selection(true, view.is_editable());
            view.scroll_mark_onscreen(&buffer.get_insert());
            view.stop_signal_emission_by_name("cut-clipboard");
            return;
        }
        if !view.is_editable() {
            return;
        }
        view.clipboard().set_text(&whole_lines(view));
        delete_line(view);
    });
    menu_items(view);
}

/// Put GTK's own Cut and Copy back on the context menu when nothing is selected, so the menu
/// reaches the whole-line idiom the chords have.
///
/// Both items activate by emitting the two signals above, so the path is already the right one;
/// what stopped them is that `gtk_text_view_do_popup` calls
/// `gtk_text_view_update_clipboard_actions` on its way to the menu, and that leaves the two
/// actions disabled while the buffer has no selection (GTK 4.22). Enabling them has to happen
/// *after* the popup that disabled them, which is what the idle is for; a menu item follows its
/// action's enabled state while it is on screen, so it is sensitive before the popover has
/// finished coming up.
fn menu_items(view: &sourceview5::View) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    click.connect_pressed(|click, _, _, _| {
        if let Some(view) = click.widget().and_downcast::<sourceview5::View>() {
            arm_menu_items(&view);
        }
    });
    view.add_controller(click);
    // The keyboard's own way to the same menu, which no gesture sees.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|keys, key, _, state| {
        let opens_menu = key == gdk::Key::Menu
            || (key == gdk::Key::F10 && state.contains(gdk::ModifierType::SHIFT_MASK));
        if opens_menu && let Some(view) = keys.widget().and_downcast::<sourceview5::View>() {
            arm_menu_items(&view);
        }
        glib::Propagation::Proceed
    });
    view.add_controller(keys);
}

/// Enable the two items once the menu this press or key is opening is up. A cut on a read-only
/// view is left out, as GTK leaves it out: there is nothing there to take away.
fn arm_menu_items(view: &sourceview5::View) {
    let view = view.clone();
    glib::idle_add_local_once(move || {
        view.action_set_enabled("clipboard.copy", true);
        view.action_set_enabled("clipboard.cut", view.is_editable());
    });
}

/// A secondary click puts the caret where it lands before the menu opens, as VS Code does, so
/// the menu is about the place clicked: Cut and Copy with nothing selected take that line, Paste
/// goes there, and the spelling suggestions are that word's. One landing on a selection, any of a
/// column's, leaves everything where it is, the menu then being about the selection; one landing
/// off them all lets a column of carets go, down to the one at the click. In the capture phase
/// and unclaimed, so GTK's own gesture still opens the menu, after this.
pub(crate) fn caret_to_click(view: &sourceview5::View) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    click.connect_pressed(|click, presses, x, y| {
        if let Some(view) = click.widget().and_downcast::<sourceview5::View>()
            && presses == 1
        {
            place_at_click(&view, x, y);
        }
    });
    view.add_controller(click);
}

/// The caret to the press at widget `x`, `y` of `view`, unless it lands on a selection
/// ([`caret_to_click`]). Placed as a middle-click paste lands ([`pressed_at`]).
pub(crate) fn place_at_click(view: &sourceview5::View, x: f64, y: f64) {
    let (x, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
    let at = pressed_at(view, x, y);
    let buffer = view.buffer();
    let column = view.downcast_ref::<multicaret::View>();
    let selections = match column {
        Some(column) => column.selections(),
        None => buffer.selection_bounds().into_iter().collect(),
    };
    if selections
        .iter()
        .any(|(start, end)| start != end && at.in_range(start, end))
    {
        return;
    }
    if let Some(column) = column {
        column.clear_carets();
    }
    buffer.place_cursor(&at);
}

/// Middle-click paste, as plain text like every other way in. GTK's own reads the primary
/// selection as a `GtkTextBuffer`, and when that is this buffer's selection it inserts it run by
/// run with each run's tags: the highlighter re-derives its own, but a fold's tag came along and
/// hid part of the paste behind no chevron. In the capture phase and claimed, so the view's own
/// gesture never sees the press.
pub(super) fn primary_paste(view: &sourceview5::View) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_MIDDLE)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    click.connect_pressed(|click, _, x, y| {
        let Some(view) = click.widget().and_downcast::<sourceview5::View>() else {
            return;
        };
        // Off, GTK's gesture does nothing with the press either.
        if !view.settings().is_gtk_enable_primary_paste() {
            return;
        }
        click.set_state(gtk::EventSequenceState::Claimed);
        view.grab_focus();
        let (x, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        paste_primary(&view, &pressed_at(&view, x, y));
    });
    view.add_controller(click);
}

/// The place a press at buffer `x`, `y` means, as GTK's own gesture takes it: the character under
/// it, or the end or start of the row it is beside. `iter_at_location` answers only over text, so
/// beside a row that row is found by walking the paragraph's display rows down to `y`.
pub(crate) fn pressed_at(view: &sourceview5::View, x: i32, y: i32) -> gtk::TextIter {
    if let Some(at) = crate::fold::iter_at_location(view, x, y) {
        return at;
    }
    let (mut row, _) = view.line_at_y(y);
    let mut next = row;
    while view.forward_display_line(&mut next)
        && next.line() == row.line()
        && view.iter_location(&next).y() <= y
    {
        row = next;
    }
    if x > view.iter_location(&row).x() {
        view.forward_display_line_end(&mut row);
    }
    row
}

/// Insert the primary selection at `at` as text, hidden text included as a copy takes it. Onto
/// the view's own selection it is nothing, as in GTK: that selection is what would be pasted.
pub(crate) fn paste_primary(view: &sourceview5::View, at: &gtk::TextIter) {
    let buffer = view.buffer();
    if buffer
        .selection_bounds()
        .is_some_and(|(start, end)| start <= *at && *at <= end)
    {
        return;
    }
    // The read is asynchronous even from this process, and the text may change meanwhile.
    let mark = buffer.create_mark(None, at, false);
    view.primary_clipboard().read_value_async(
        gtk::TextBuffer::static_type(),
        glib::Priority::DEFAULT,
        gio::Cancellable::NONE,
        glib::clone!(
            #[weak]
            view,
            move |value| {
                let buffer = view.buffer();
                let mut at = buffer.iter_at_mark(&mark);
                buffer.delete_mark(&mark);
                let Some(source) = value.ok().and_then(|v| v.get::<gtk::TextBuffer>().ok()) else {
                    return;
                };
                let Some((start, end)) = source.selection_bounds() else {
                    return;
                };
                let text = source.text(&start, &end, true);
                buffer.begin_user_action();
                buffer.insert_interactive(&mut at, &text, view.is_editable());
                buffer.end_user_action();
            }
        ),
    );
}

/// `text` as a snippet whose stops are the byte offsets `stops`, in Tab order.
fn snippet(text: &str, stops: &[usize]) -> sourceview5::Snippet {
    let snippet = sourceview5::Snippet::new(None, None);
    for (piece, focus) in chunks(text, stops) {
        let chunk = sourceview5::SnippetChunk::new();
        chunk.set_text(piece);
        chunk.set_text_set(true);
        chunk.set_focus_position(focus);
        snippet.add_chunk(&chunk);
    }
    snippet
}

/// The text between the stops, then each stop as an empty chunk numbered from one; plain text
/// carries -1, which is what GtkSourceView reads as "not a stop". A stop off a character
/// boundary, which the renderer never produces, is skipped rather than trusted.
///
/// Last, an empty chunk numbered 0 at the end, which is where Tab past the last stop goes.
/// Without one GtkSourceView 5.20 moves the caret there itself with no chunk current, and its own
/// caret handler then fails an assertion (`_gtk_source_snippet_insert_set`).
fn chunks<'a>(text: &'a str, stops: &[usize]) -> Vec<(&'a str, i32)> {
    let mut out = Vec::new();
    let mut byte = 0;
    let mut focus = 0;
    for &stop in stops {
        let Some(piece) = text.get(byte..stop) else {
            continue;
        };
        if !piece.is_empty() {
            out.push((piece, -1));
        }
        focus += 1;
        out.push(("", focus));
        byte = stop;
    }
    if byte < text.len() {
        out.push((&text[byte..], -1));
    }
    out.push(("", 0));
    out
}

impl Tab {
    /// Wrap long lines, or stop. Every tab starts wrapped and can be told otherwise for as long
    /// as it is open, which is the escape hatch for a file whose columns are the point.
    pub fn toggle_wrap(&self) {
        self.view.set_wrap_mode(match self.view.wrap_mode() {
            gtk::WrapMode::None => gtk::WrapMode::WordChar,
            _ => gtk::WrapMode::None,
        });
    }

    // --- line operations -----------------------------------------------------------------

    /// [`newline_below`] in this tab.
    pub fn newline_below(&self) {
        newline_below(&self.view);
    }

    /// [`duplicate_line`] in this tab.
    pub fn duplicate_line(&self) {
        duplicate_line(&self.view);
    }

    /// [`delete_line`] in this tab.
    pub fn delete_line(&self) {
        delete_line(&self.view);
    }

    /// [`toggle_comment`] in this tab.
    pub fn toggle_comment(&self) {
        toggle_comment(&self.view);
    }

    /// Put the caret on a template's first `{{cursor}}` and make the rest Tab stops.
    ///
    /// GtkSourceView can only walk stops through text a snippet inserted, so with more than one
    /// the note's text is put back through a snippet: the same bytes, not undoable and not dirty,
    /// because nothing about the file changed. `byte` offsets, as the template renderer counts.
    pub fn place_stops(&self, stops: &[usize]) {
        let text = self.text();
        match stops {
            [] => {}
            [only] => {
                let at = text.get(..*only).map_or(0, |s| s.chars().count());
                self.goto_range(at..at);
            }
            _ => {
                let (mut start, mut end) = self.buffer.bounds();
                self.loading.set(true);
                self.buffer.begin_irreversible_action();
                self.buffer.delete(&mut start, &mut end);
                self.push_snippet(&snippet(&text, stops), &mut self.buffer.start_iter());
                self.buffer.end_irreversible_action();
                self.loading.set(false);
                self.buffer.set_modified(false);
                self.analyse();
                // The snippet put the caret on the first stop; a tab still opening scrolls there
                // once it is laid out, as a jump does.
                self.scroll_to_caret(0.3, false);
            }
        }
    }

    /// A template's text at the caret, with its `{{cursor}}` stops for Tab to walk.
    pub fn insert_stops(&self, text: &str, stops: &[usize]) {
        let mut at = caret(&self.buffer);
        match stops.is_empty() {
            true => self.buffer.insert(&mut at, text),
            false => self.push_snippet(&snippet(text, stops), &mut at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_split_the_text_at_its_stops_in_order() {
        assert_eq!(
            chunks("ab: \ncd: \n", &[4, 9]),
            [
                ("ab: ", -1),
                ("", 1),
                ("\ncd: ", -1),
                ("", 2),
                ("\n", -1),
                ("", 0)
            ]
        );
        assert_eq!(chunks("x", &[0]), [("", 1), ("x", -1), ("", 0)]);
        assert_eq!(chunks("x", &[1]), [("x", -1), ("", 1), ("", 0)]);
        assert_eq!(
            chunks("ab", &[0, 0]),
            [("", 1), ("", 2), ("ab", -1), ("", 0)]
        );
    }

    #[test]
    fn leading_indent_is_the_spaces_and_tabs_a_line_opens_with() {
        assert_eq!(leading_indent(""), "");
        assert_eq!(leading_indent("  - a"), "  ");
        assert_eq!(leading_indent("\tx"), "\t");
        assert_eq!(leading_indent("no indent\n"), "");
        assert_eq!(
            leading_indent("   \n"),
            "   ",
            "a blank line still has its indent"
        );
    }

    /// What a whole-line cut or copy puts on the clipboard: a line, newline included, so the
    /// paste that follows it opens a line of its own.
    #[test]
    fn a_copied_line_carries_its_newline() {
        assert_eq!(paste_ready("- item\n"), "- item\n");
        assert_eq!(
            paste_ready("last line"),
            "last line\n",
            "a last line has none"
        );
        assert_eq!(paste_ready("\n"), "\n", "an empty line is still a line");
    }

    /// What the line commands take at a column: every line a caret covers, each once, and carets
    /// on neighbouring lines two runs rather than one.
    #[test]
    fn covered_lines_are_taken_once_and_neighbours_stay_apart() {
        assert_eq!(runs(vec![(2, 2), (0, 0), (1, 1)]), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(runs(vec![(0, 3), (2, 5), (5, 5)]), [(0, 5)]);
        assert_eq!(runs(vec![(4, 4), (4, 4)]), [(4, 4)]);
    }

    /// A comment marker going on or coming off changes one run of a line, and the head and tail
    /// around it are what a caret in the line hangs on to.
    #[test]
    fn a_toggled_line_is_replaced_only_where_it_differs() {
        assert_eq!(shared("    a();", "    // a();"), (4, 4));
        assert_eq!(shared("    // a();", "    a();"), (4, 4));
        assert_eq!(shared("x", "x"), (1, 0), "nothing to replace");
        assert_eq!(shared("", "// "), (0, 0));
        assert_eq!(shared("aa", "aaa"), (2, 0), "the two runs never overlap");
    }

    #[test]
    fn duplicate_copies_every_line_the_selection_touches() {
        assert_eq!(last_copied(2, 2, 5), 2, "the caret's line");
        assert_eq!(last_copied(2, 2, 0), 2, "even at its start");
        assert_eq!(last_copied(0, 1, 2), 1);
        assert_eq!(
            last_copied(0, 2, 0),
            1,
            "a selection ending at a line's start leaves that line out"
        );
    }
}
