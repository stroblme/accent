//! A code block's text, set plain by [`super::text`], coloured as the editor colours it
//! (`codeblocks.rs`, whose runs and tags it shares): at once where its language and code have runs
//! kept already, else once the page's text is all in, from an idle. A restyle gives the tags the
//! theme's looks.

use super::Page;
use super::text::Writer;
use crate::codeblocks;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::ops::Range;

/// A code block still to be highlighted.
struct Waiting {
    /// Where its code starts. Right gravity: a block written anew above it leaves it on the code.
    mark: gtk::TextMark,
    language: sourceview5::Language,
    code: String,
}

thread_local! {
    /// The code blocks the previews have written since an idle last took them.
    static WAITING: RefCell<Vec<Waiting>> = RefCell::default();
}

/// The code block in `lang` the writer just put at `range` (offsets from its block's start).
pub(super) fn fence(w: &mut Writer, lang: &str, range: Range<i32>) {
    // The language as the editor reads a fence's: `rust` of `rust,ignore`.
    let name = lang.split([',', '{']).next().unwrap_or_default();
    let Some(language) = codeblocks::language(name) else {
        return;
    };
    let buffer = &w.page.buffer;
    let start = buffer.iter_at_offset(w.at.offset() - (range.end - range.start));
    let code = buffer.slice(&start, w.at, true).to_string();
    match codeblocks::runs(&language, &code, false) {
        Some(runs) => paint(buffer, &start, &runs),
        None => wait(Waiting {
            mark: buffer.create_mark(None, &start, false),
            language,
            code,
        }),
    }
}

/// The theme changed: the code's tags take its looks.
pub(super) fn restyle(page: &Page) {
    codeblocks::restyle(&page.buffer);
}

/// Highlight `waiting` with the blocks written before an idle below the render's batches runs,
/// and colour each that is still on show as it was written.
fn wait(waiting: Waiting) {
    let first = WAITING.with_borrow_mut(|all| {
        all.push(waiting);
        all.len() == 1
    });
    if !first {
        return;
    }
    glib::idle_add_local_full(glib::Priority::LOW, || {
        let waiting = WAITING.take();
        let blocks = (waiting.iter())
            .map(|w| (w.language.clone(), w.code.clone()))
            .collect();
        codeblocks::later(blocks, move || waiting.iter().for_each(Waiting::paint));
        glib::ControlFlow::Break
    });
}

impl Waiting {
    /// Colour the block if its code is still on show where it was written: a render that wrote it
    /// anew coloured it itself. Highlighted again if the runs kept were dropped meanwhile.
    fn paint(&self) {
        let Some(buffer) = self.mark.buffer() else {
            return;
        };
        let start = buffer.iter_at_mark(&self.mark);
        let mut end = start;
        end.forward_chars(self.code.chars().count() as i32);
        if buffer.slice(&start, &end, true) == self.code
            && let Some(runs) = codeblocks::runs(&self.language, &self.code, true)
        {
            paint(&buffer, &start, &runs);
        }
        buffer.delete_mark(&self.mark);
    }
}

/// Lend each run's look to the code starting at `start`.
fn paint(buffer: &gtk::TextBuffer, start: &gtk::TextIter, runs: &codeblocks::Runs) {
    let table = buffer.tag_table();
    let at = |offset: i32| buffer.iter_at_offset(start.offset() + offset);
    for (range, name) in runs.iter() {
        if let Some(tag) = codeblocks::tag(&table, name) {
            buffer.apply_tag(&tag, &at(range.start), &at(range.end));
        }
    }
}
