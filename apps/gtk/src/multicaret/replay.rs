//! A column of carets edited as one: a key replayed at every caret, the undo steps the column
//! records, and carets merging where their selections meet.

use super::*;

impl View {
    /// Up or Down by `count` lines of the document. The column the caret is aiming for outlives
    /// the lines it crosses, so a trip over a short line and back lands where it started.
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

    /// One key press, at every caret. Called by `editor::keys`, and by the headless check that
    /// drives the carets the way that dispatcher does, which is the only way to see them without
    /// a screen.
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

    /// How far Page Up and Page Down move a column: the lines of the document on screen, a shut
    /// block's header without the lines it hides. And where the primary caret sits among them, 0
    /// at the top and 1 at the bottom, so the view can scroll by the same page and leave it there.
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

    /// Move one caret to `at`, its anchor with it unless `extend` holds the anchor where it is.
    /// The primary through GTK's own calls: `move_mark` on the insert mark alone extends its
    /// selection, and `place_cursor` carries the selection bound along.
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

    /// Open one undo step made at every caret: the edits and caret moves from here on are the
    /// column's own, and where the carets and their selections stood is kept for Undo.
    fn begin_step(&self) -> (Vec<Span>, i32) {
        let before = (self.spans(), self.buffer().char_count());
        self.imp().busy.set(true);
        self.buffer().begin_user_action();
        before
    }

    /// Close the step [`Self::begin_step`] opened: record it for Undo and Redo if it edited the
    /// text, merge the carets it drove together and settle the column.
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

    /// Solid again from here, back to GTK's caret if the column has collapsed into one, and the
    /// primary caret on screen.
    fn settle(&self) {
        match self.has_carets() {
            true => self.blink_on(),
            false => self.blink_off(),
        }
        self.scroll_mark_onscreen(&self.buffer().get_insert());
        self.queue_draw();
    }

    /// `op` once on each run of lines in `runs`, from the bottom up so that a run's line numbers
    /// still hold when its turn comes, as one undo step: Duplicate Line, Delete Line and Insert
    /// Line Below at a column (`editor::lines`, which says what a run is). `op` edits lines
    /// `first` to `last` and answers where the carets whose selections start in them go, or
    /// `None` where the edit already took them there.
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

    /// Put `pieces` at the carets top to bottom, each in place of its caret's selection, as one
    /// undo step. Refused on a read-only view, as [`Self::replay`] is.
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

    /// Undo (`back`) or Redo at a column, which keeps the column. GTK puts the text back and one
    /// caret with it; the column's record of the step puts every caret and its selection where the
    /// step found them, or, going forward, where it left them. A step with no record — made
    /// before the column — leaves the carets wherever the text took them.
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

    /// The carets of the step the text has just come back to, its record moved onto the other
    /// stack along with any record GTK took back with it.
    ///
    /// GTK can put back more than one of the column's steps at once: an edit that changed the text
    /// at a single caret — `Ctrl+Delete` with another caret at the end of the buffer, Backspace
    /// with one at the start — is a plain action in its history rather than a group, and it joins
    /// a plain action onto the one before it the way it joins typing. The column keeps a record
    /// per edit either way, so records are taken off until one of them describes the text that
    /// came back, by the length [`Self::end_step`] recorded it at. Every record taken off moves to
    /// the other stack, so the way forward takes the same ones back.
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

    /// Put the column back as `spans` has it, the primary first. Made afresh, since carets that
    /// merged after the step was recorded are back.
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
