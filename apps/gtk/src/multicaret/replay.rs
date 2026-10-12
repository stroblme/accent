//! Replay edits at all carets in one undo step, preserving selection state
//! and merging overlapping carets.

use super::*;

impl View {
    /// Move by `count` document lines while retaining the goal column across
    /// shorter lines, so returning restores the original column.
    pub(super) fn move_by_lines(&self, count: i32, extend: bool) {
        let imp = self.imp();
        self.reset_im_context();
        let buffer = self.buffer();
        let insert = buffer.get_insert();
        let from = buffer.iter_at_mark(&insert);
        let (line, reached) = crate::fold::line_by(&from, count);
        let (landing, goal) = vertical_step(
            imp.goal.get(),
            from.line_offset(),
            line_length(&buffer, line),
        );
        let at = match reached {
            true => {
                let mut at = buffer.iter_at_line(line).unwrap_or(from);
                at.set_line_offset(landing);
                at
            }
            // Past the last line shown the caret parks at its end, or at the start of the first,
            // which is what GTK does and what keeps Down at the bottom doing something.
            false if count > 0 => line_end(&buffer, line),
            false => buffer.iter_at_line(line).unwrap_or(from),
        };
        imp.goal.set(Some(goal));

        // Ours, not the user's: the `mark-set` hook would read it as a click and drop both the
        // goal we just set and every secondary caret.
        imp.busy.set(true);
        match extend {
            true => buffer.move_mark(&insert, &at),
            false => buffer.place_cursor(&at),
        }
        imp.busy.set(false);
        self.scroll_mark_onscreen(&insert);
    }

    /// Dispatch one press at every caret, shared by `editor::keys` and
    /// headless drills.
    pub(crate) fn press(&self, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if !self.has_carets() {
            return glib::Propagation::Proceed;
        }
        if ends_column(key, state) {
            self.clear_carets();
            // Escape is the column's: it ends the column and leaves the primary's selection, and
            // only a second one is GTK's. The keys an input method finishes have to reach it.
            return match key == gdk::Key::Escape {
                true => glib::Propagation::Stop,
                false => glib::Propagation::Proceed,
            };
        }
        if let Some(back) = undo_or_redo(key, state) {
            self.step_history(back);
            return glib::Propagation::Stop;
        }
        match edit_for(key, state) {
            Some(edit) => {
                self.replay(&edit);
                glib::Propagation::Stop
            }
            // GTK's, with the column left up: what GTK does ends it only if it moves a caret or
            // edits, which the hooks in `constructed` see.
            None => glib::Propagation::Proceed,
        }
    }

    /// Apply `edit` at every caret as one undoable step.
    fn replay(&self, edit: &Edit) {
        // A read-only view still moves its carets, as GTK's own one does, but nothing writes to
        // it: these edits go in through the buffer, which has no view to ask about that itself.
        if !matches!(edit, Edit::Move(..)) && !self.is_editable() {
            return;
        }
        let buffer = self.buffer();
        let insert = buffer.get_insert();
        let imp = self.imp();
        // Marks and goal column per caret, the primary appended so it is edited like any other.
        // Copied out of the cells first: the edits below move marks, and a borrow held across
        // them would meet the hooks that fire on the way.
        let mut carets: Vec<(gtk::TextMark, gtk::TextMark, Option<i32>)> = imp
            .carets
            .borrow()
            .iter()
            .map(|caret| (caret.mark.clone(), caret.anchor.clone(), caret.goal))
            .collect();
        carets.push((insert.clone(), buffer.selection_bound(), imp.goal.get()));
        // Measured before anything moves.
        let page =
            matches!(edit, Edit::Move(Motion::PageUp | Motion::PageDown, _)).then(|| self.page());
        let page_lines = page.map_or(0, |(lines, _)| lines);

        let opened = self.begin_step();
        for (mark, anchor, goal) in &mut carets {
            let mut at = buffer.iter_at_mark(mark);
            let span = Span {
                anchor: buffer.iter_at_mark(anchor).offset(),
                caret: at.offset(),
            };
            // An edit takes the caret's selection first: what is typed goes in its place, and a
            // delete takes the selection and nothing more.
            if !matches!(edit, Edit::Move(..)) && !span.is_empty() {
                let mut to = buffer.iter_at_offset(span.end());
                at = buffer.iter_at_offset(span.start());
                buffer.delete(&mut at, &mut to);
            }
            // Only vertical movement leaves a column behind to aim at; everything else drops it.
            let mut aim = None;
            match edit {
                Edit::Insert(text) => buffer.insert(&mut at, text),
                // The view already knows what Tab means here — `editor.rs` sets both properties
                // for code and leaves a note with its literal tab — so every caret answers the
                // way the primary one does, each from the column it is actually in.
                Edit::Tab => {
                    let width = self.tab_width() as usize;
                    let column = visual_column(&line_prefix(&buffer, &at), width);
                    let text = tab_insert(column, width, self.is_insert_spaces_instead_of_tabs());
                    buffer.insert(&mut at, &text);
                }
                Edit::Backspace | Edit::Delete | Edit::DeleteWord(_) if !span.is_empty() => {}
                Edit::Backspace => {
                    let mut from = at;
                    if from.backward_char() {
                        buffer.delete(&mut from, &mut at);
                    }
                }
                Edit::Delete => {
                    let mut to = at;
                    if to.forward_char() {
                        buffer.delete(&mut at, &mut to);
                    }
                }
                // The same function the primary caret's `Ctrl+Delete` goes through, so the two
                // cannot drift apart.
                Edit::DeleteWord(forward) => {
                    let (mut from, mut to) = word_range(&buffer, at, *forward);
                    buffer.delete(&mut from, &mut to);
                }
                Edit::Move(motion, extend) => {
                    // Shift moves the caret from where it is. Without it a selection is left
                    // from one of its ends, or only collapsed onto one ([`departure`]).
                    let (from, goes_on) = match extend {
                        true => (span.caret, true),
                        false => departure(*motion, span),
                    };
                    at = buffer.iter_at_offset(from);
                    match motion {
                        _ if !goes_on => {}
                        Motion::Left => {
                            at.backward_char();
                        }
                        Motion::Right => {
                            at.forward_char();
                        }
                        Motion::WordLeft => {
                            at.backward_visible_word_start();
                        }
                        Motion::WordRight => {
                            at.forward_visible_word_end();
                        }
                        Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                            let step = match motion {
                                Motion::Up => -1,
                                Motion::Down => 1,
                                Motion::PageUp => -page_lines,
                                _ => page_lines,
                            };
                            // A shut block is one line, as at a single caret, and a page stops at
                            // either end of the document: a line step there lands where the
                            // caret already is.
                            let (line, _) = crate::fold::line_by(&at, step);
                            if let Some(mut moved) = buffer.iter_at_line(line) {
                                let (column, kept) = vertical_step(
                                    *goal,
                                    at.line_offset(),
                                    line_length(&buffer, line),
                                );
                                moved.set_line_offset(column);
                                at = moved;
                                aim = Some(kept);
                            }
                        }
                        Motion::Home => at.set_line_offset(0),
                        Motion::End => at = line_end(&buffer, at.line()),
                    }
                    self.put(mark, anchor, &at, *extend);
                }
            }
            *goal = aim;
        }
        // Back into the cells the goals came out of; the primary's is the one pushed last.
        imp.goal.set(carets.pop().and_then(|(_, _, goal)| goal));
        for (caret, (_, _, goal)) in imp.carets.borrow_mut().iter_mut().zip(&carets) {
            caret.goal = *goal;
        }
        self.end_step(opened);
        // Where GTK's own page keys leave the caret: as high up the screen as it was, a page on.
        if let Some((_, align)) = page {
            self.scroll_to_mark(&insert, 0.0, true, 0.0, align);
        }
    }

    /// Return visible document-line count for paging, excluding folded bodies,
    /// and the primary's viewport fraction (0 top, 1 bottom) to retain it
    /// after scrolling the same page.
    fn page(&self) -> (i32, f64) {
        let rect = self.visible_rect();
        let (mut row, _) = self.line_at_y(rect.y());
        let (bottom, _) = self.line_at_y(rect.y() + rect.height());
        let y = self.iter_location(&caret(&self.buffer())).y() - rect.y();
        let align = f64::from(y) / f64::from(rect.height().max(1));
        let mut lines = 0;
        while crate::fold::visible_line(&mut row, true) && row.line() <= bottom.line() {
            lines += 1;
        }
        (lines.max(1), align.clamp(0.0, 1.0))
    }

    /// Move a caret to `at`, retaining its anchor only with `extend`. Use GTK
    /// primary-caret calls: moving insert alone extends; `place_cursor`
    /// moves both insert and selection bound.
    fn put(&self, mark: &gtk::TextMark, anchor: &gtk::TextMark, at: &gtk::TextIter, extend: bool) {
        let buffer = self.buffer();
        if *mark == buffer.get_insert() && !extend {
            return buffer.place_cursor(at);
        }
        buffer.move_mark(mark, at);
        if !extend {
            buffer.move_mark(anchor, at);
        }
    }

    /// The lines the carets are on, each once, top to bottom.
    pub(crate) fn caret_lines(&self) -> Vec<i32> {
        let buffer = self.buffer();
        let mut lines: Vec<i32> = self
            .pairs()
            .iter()
            .map(|(mark, _)| buffer.iter_at_mark(mark).line())
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Begin a column-owned undo step and retain carets/selections for Undo.
    fn begin_step(&self) -> (Vec<Span>, i32) {
        let before = (self.spans(), self.buffer().char_count());
        self.imp().busy.set(true);
        self.buffer().begin_user_action();
        before
    }

    /// Finish the step: record text edits for undo/redo, merge converged
    /// carets and settle the column.
    fn end_step(&self, (before, length): (Vec<Span>, i32)) {
        let buffer = self.buffer();
        let imp = self.imp();
        buffer.end_user_action();
        imp.busy.set(false);
        self.collapse();
        // Every column edit inserts or deletes, so GTK recorded a step exactly when the length
        // of the text moved.
        if buffer.char_count() != length {
            let after = self.spans();
            imp.undo.borrow_mut().push(Some(imp::Step {
                before,
                after,
                lengths: (length, buffer.char_count()),
            }));
            imp.redo.take();
        }
        self.settle();
    }

    /// Restore solid carets, hand back to GTK if only one remains and
    /// keep the primary visible.
    fn settle(&self) {
        match self.has_carets() {
            true => self.blink_on(),
            false => self.blink_off(),
        }
        self.scroll_mark_onscreen(&self.buffer().get_insert());
        self.queue_draw();
    }

    /// Apply `op` to each inclusive line run bottom-up in one undo step,
    /// preserving earlier line numbers. Return the destination for carets
    /// whose selections start in the run, or `None` if edits already moved
    /// them. Used by column commands in `editor::lines`.
    pub(crate) fn each_block(
        &self,
        runs: &[(i32, i32)],
        op: impl Fn(&gtk::TextBuffer, i32, i32) -> Option<gtk::TextIter>,
    ) {
        let buffer = self.buffer();
        let pairs = self.pairs();
        // The line each selection starts on, read before anything moves.
        let starts: Vec<i32> = self
            .spans()
            .iter()
            .map(|span| buffer.iter_at_offset(span.start()).line())
            .collect();
        let opened = self.begin_step();
        for &(first, last) in runs.iter().rev() {
            let Some(to) = op(&buffer, first, last) else {
                continue;
            };
            for ((mark, anchor), line) in pairs.iter().zip(&starts) {
                if (first..=last).contains(line) {
                    self.put(mark, anchor, &to, false);
                }
            }
        }
        self.end_step(opened);
    }

    /// Paste `text` at every caret as one undo step, a line each where it [`spread`]s.
    pub(super) fn paste_at_carets(&self, text: &str) {
        let pieces = spread(text, self.pairs().len());
        self.replace_selections(&pieces);
    }

    /// Delete every caret's selection as one undo step: a cut at a column (`editor::lines`).
    pub(crate) fn delete_selections(&self) {
        self.replace_selections(&vec![""; self.pairs().len()]);
    }

    /// Replace selections with `pieces` top to bottom in one undo step.
    /// Refuse read-only views, as [`Self::replay`] does.
    fn replace_selections(&self, pieces: &[&str]) {
        if !self.is_editable() {
            return;
        }
        let buffer = self.buffer();
        let mut pairs = self.pairs();
        pairs.sort_by_key(|(mark, _)| buffer.iter_at_mark(mark).offset());
        let opened = self.begin_step();
        for ((mark, anchor), piece) in pairs.iter().zip(pieces) {
            let mut from = buffer.iter_at_mark(mark);
            let mut to = buffer.iter_at_mark(anchor);
            buffer.delete(&mut from, &mut to);
            buffer.insert(&mut from, piece);
        }
        self.end_step(opened);
    }

    /// Undo/redo while retaining the column. GTK restores text and one caret;
    /// our record restores all selections before/after the step. Unrecorded
    /// steps predating the column leave carets where restored text carries them.
    fn step_history(&self, back: bool) {
        let buffer = self.buffer();
        let imp = self.imp();
        let can = match back {
            true => buffer.can_undo(),
            false => buffer.can_redo(),
        };
        if !can {
            return;
        }
        imp.busy.set(true);
        match back {
            true => buffer.undo(),
            false => buffer.redo(),
        }
        imp.busy.set(false);
        if let Some(spans) = self.take_step(back) {
            self.put_carets(&spans);
        }
        self.collapse();
        self.settle();
    }

    /// Restore carets for the text reached by Undo or Redo and transfer records between stacks.
    ///
    /// GTK can merge single-caret-effective edits into one history action. Pop column records
    /// until [`Self::end_step`]'s recorded length matches restored text, moving all popped records
    /// so Redo follows the same grouped steps.
    fn take_step(&self, back: bool) -> Option<Vec<Span>> {
        let length = self.buffer().char_count();
        let imp = self.imp();
        let (from, to) = match back {
            true => (&imp.undo, &imp.redo),
            false => (&imp.redo, &imp.undo),
        };
        loop {
            // A step from before the column, or none of the column's left: nothing to put back.
            let Some(step) = from.borrow_mut().pop().flatten() else {
                to.borrow_mut().push(None);
                return None;
            };
            let landed = match back {
                true => (step.lengths.0 == length).then(|| step.before.clone()),
                false => (step.lengths.1 == length).then(|| step.after.clone()),
            };
            to.borrow_mut().push(Some(step));
            if landed.is_some() {
                return landed;
            }
        }
    }

    /// Rebuild the primary-first column from `spans`, restoring carets
    /// merged since the recorded step.
    pub(super) fn put_carets(&self, spans: &[Span]) {
        let buffer = self.buffer();
        let imp = self.imp();
        let at = |offset| buffer.iter_at_offset(offset);
        let Some((primary, rest)) = spans.split_first() else {
            return;
        };
        imp.busy.set(true);
        buffer.select_range(&at(primary.caret), &at(primary.anchor));
        imp.busy.set(false);
        let carets = rest
            .iter()
            .map(|span| imp::Caret::new(&buffer, &at(span.anchor), &at(span.caret)))
            .collect();
        for caret in imp.carets.replace(carets) {
            caret.delete(&buffer);
        }
    }

    /// Carets whose selections overlap are one caret from here on ([`merge`]), two driven onto
    /// the same character the plainest case.
    pub(super) fn collapse(&self) {
        let buffer = self.buffer();
        let imp = self.imp();
        let spans = self.spans();
        let mut kept: Vec<Option<Span>> = vec![None; spans.len()];
        for (index, span) in merge(&spans) {
            kept[index] = Some(span);
        }
        if kept
            .iter()
            .zip(&spans)
            .all(|(kept, span)| *kept == Some(*span))
        {
            return;
        }
        let at = |offset| buffer.iter_at_offset(offset);
        // The primary survives every merge it is in.
        if let Some(span) = kept[0] {
            imp.busy.set(true);
            buffer.select_range(&at(span.caret), &at(span.anchor));
            imp.busy.set(false);
        }
        let carets: Vec<imp::Caret> = imp.carets.take();
        let survivors = carets
            .into_iter()
            .zip(&kept[1..])
            .filter_map(|(caret, span)| match span {
                Some(span) => {
                    buffer.move_mark(&caret.mark, &at(span.caret));
                    buffer.move_mark(&caret.anchor, &at(span.anchor));
                    Some(caret)
                }
                None => {
                    caret.delete(&buffer);
                    None
                }
            })
            .collect();
        imp.carets.replace(survivors);
    }
}
