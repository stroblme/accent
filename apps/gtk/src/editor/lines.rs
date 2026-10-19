//! Whole-line cut/copy, insertion, duplication, deletion, comments and wrapping.
//! Line commands apply at every caret; plain-text middle-click paste avoids
//! carrying buffer tags. Templates use snippet Tab stops.

use super::{Tab, caret, line_end};
use crate::{comment, multicaret};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use sourceview5::prelude::*;

/// Return the leading spaces and tabs copied to a new line below.
pub(super) fn leading_indent(line: &str) -> &str {
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// Last selected line for Duplicate Line, excluding a line touched only
/// at column zero. This keeps Shift+Down whole-line selections from
/// copying the following line (VS Code semantics).
fn last_copied(first: i32, last: i32, end_column: i32) -> i32 {
    match last > first && end_column == 0 {
        true => last - 1,
        false => last,
    }
}

/// Include a trailing newline even for the final line, so whole-line
/// paste inserts a line instead of splicing into existing text.
fn paste_ready(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("{line}\n"),
    }
}

/// Read `line` including its newline, except for an unterminated final line.
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

/// Sort and merge inclusive line ranges that overlap, avoiding duplicate
/// operations. Keep merely adjacent ranges separate.
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

/// Line runs covered by column selections ([`last_copied`]), using each
/// caret's line when it has no selection.
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

/// Inclusive selected-line range ([`last_copied`]), or the caret's line
/// without selection, for commands outside a caret column.
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

/// Column clipboard text: join selections top to bottom, including empty
/// caret selections, or copy whole lines when no caret selects text.
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

/// Copy each caret's whole line once in top-to-bottom order when no text
/// is selected, following VS Code semantics.
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

/// Delete inclusive lines and their newlines. For an unterminated final
/// line, remove the preceding newline too so no blank line remains.
fn delete_lines(buffer: &gtk::TextBuffer, first: i32, last: i32) {
    let (mut start, _) = line_bounds(buffer, first);
    let (_, mut end) = line_bounds(buffer, last);
    if !buffer.text(&start, &end, true).ends_with('\n') {
        start.backward_char();
    }
    buffer.delete(&mut start, &mut end);
}

/// Duplicate lines `first` through `last` below themselves.
///
/// Insert before the original lines so right-gravity caret and selection marks move to the
/// lower block. This also handles a final line without a newline.
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

/// Duplicate covered lines below and move caret/selection to the copy
/// at the same columns (VS Code Copy Line Down). At a caret column,
/// apply to each [`covered`] run.
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

/// Delete selected lines ([`selected_lines`]) or each column-covered line
/// once, using caret lines when nothing is selected.
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

/// Insert an equally indented line below, moving the caret there without
/// continuing list markers; this also lets users leave a list. At a column,
/// insert below each covered run and move its carets onto the new line.
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

/// Toggle language comment markers in `text` via [`crate::comment`], since
/// GtkSourceView metadata provides markers without a toggle. `None` means
/// no markers. `on` forces line-comment state; otherwise infer it from
/// whether all text is already commented.
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

/// Toggle comments on whole lines, replacing only changed characters to
/// preserve caret columns. `on` follows [`toggled`].
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

/// Count nonoverlapping shared prefix/suffix characters, leaving only
/// the changed middle for replacement.
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

/// Replace `old` between `start` and `end` with `new`, touching only
/// the differing middle ([`shared`]) to preserve marks in shared text.
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

/// Toggle selected or column-covered lines in one undo step. Retain the
/// column and choose line-comment state once across all runs, so a mixed
/// selection becomes fully commented (VS Code semantics).
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

/// Cut and copy plain text, using whole lines when no caret has a selection.
///
/// Multi-caret operations combine selections or lines and cut in one user action, retaining
/// the column. Signal handlers cover shortcuts and menus without application accelerators.
/// Bypass GTK's rich-buffer copy so pasted tags cannot override fresh highlighting.
/// Whole-line cut leaves the caret at the following line's start.
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

/// Enable Cut and Copy in the context menu without a selection for whole-line operations.
///
/// GTK 4.22 disables the actions while building the popup. Re-enable from an idle afterwards;
/// menu items follow action state before the popover finishes appearing.
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

/// Enable menu Cut/Copy after popup creation; omit Cut for read-only views.
fn arm_menu_items(view: &sourceview5::View) {
    let view = view.clone();
    glib::idle_add_local_once(move || {
        view.action_set_enabled("clipboard.copy", true);
        view.action_set_enabled("clipboard.cut", view.is_editable());
    });
}

/// Move the caret on secondary click before opening the context menu.
///
/// Clicks inside any selection preserve it; clicks outside a caret column end it and target
/// the clicked line. An unclaimed capture handler lets GTK open its normal menu afterwards.
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

/// Move the caret to widget coordinates unless inside a selection
/// ([`caret_to_click`]), using the paste-position rule ([`pressed_at`]).
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

/// Claim middle-click paste in capture and insert plain text. GTK rich
/// buffer paste carries tags, including folds that would hide pasted
/// text without a chevron.
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

/// Resolve buffer coordinates to a character or adjacent row start/end,
/// matching GTK. Outside text, walk paragraph display rows to `y` because
/// `iter_at_location` answers only over glyphs.
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

/// Insert primary-selection plain text at `at`, including hidden text.
/// As in GTK, do nothing when targeting the view's own selection.
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

/// Build snippet chunks: plain text numbered -1 and empty stops numbered from one.
///
/// Skip invalid character-boundary stops. End with an empty stop 0: GtkSourceView 5.20 can
/// otherwise fail its current-chunk assertion when Tab leaves the final stop.
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
    /// Toggle per-tab wrapping, enabled initially. Disable it for files whose
    /// column layout must remain visible.
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

    /// Place the caret at the first template `{{cursor}}` and turn later
    /// byte offsets into Tab stops. GtkSourceView requires snippet-inserted
    /// text, so reinsert unchanged bytes without undo or dirty state.
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

    /// Verify whole-line clipboard text ends in a newline so paste creates a line.
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

    /// Verify column line commands visit each line once and keep adjacent
    /// caret runs separate.
    #[test]
    fn covered_lines_are_taken_once_and_neighbours_stay_apart() {
        assert_eq!(runs(vec![(2, 2), (0, 0), (1, 1)]), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(runs(vec![(0, 3), (2, 5), (5, 5)]), [(0, 5)]);
        assert_eq!(runs(vec![(4, 4), (4, 4)]), [(4, 4)]);
    }

    /// Verify comment edits preserve shared text around markers for caret tracking.
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
