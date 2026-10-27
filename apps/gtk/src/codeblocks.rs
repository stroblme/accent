//! A note's blocks of code coloured as the language they name, ```` ```python ```` and
//! `#+begin_src python` alike, in a code tab's colours: GtkSourceView highlights a block's code in
//! a buffer of that language that is never shown, and every tag it puts there lends its look to a
//! tag of the note's over the same characters. A note has no GtkSourceView language of its own
//! (DESIGN.md, Editor), so where a block starts and ends is the core's reading of the note
//! ([`accent_core::note::Syntax::code_blocks`]), as for the rest of its styling.
//!
//! A block is highlighted again only when its code changes, the runs being kept by its language and
//! its code, and a pass highlights for [`BUDGET`] at most, the blocks nearest the caret first: the
//! rest are highlighted in idle time and coloured by one more pass. A theme change copies the
//! looks again, GtkSourceView restyling its own tags in place.

use crate::highlight::Offsets;
use accent_core::markdown::CodeBlock;
use accent_core::note::Syntax;
use gtk::glib;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How many blocks' runs are kept before they are all dropped and read again as needed.
const KEPT: usize = 512;
/// How long a pass, or a turn of the idle job after it, spends highlighting blocks: half a frame.
const BUDGET: Duration = Duration::from_millis(8);

/// A block's runs: a range of its code, in characters, and the note tag that colours it.
type Runs = Rc<Vec<(Range<i32>, String)>>;

#[derive(Default)]
struct State {
    /// A buffer per language, never shown, that GtkSourceView highlights a block's code in.
    hidden: HashMap<String, sourceview5::Buffer>,
    /// Each note tag's name and the hidden buffer's tag whose look it copies.
    looks: Vec<(String, gtk::TextTag)>,
    /// A block's runs, by its language and a hash of its code.
    runs: HashMap<(String, u64), Runs>,
    /// The buffers an idle job is highlighting blocks for, by address.
    pending: HashSet<usize>,
    /// The language each name a block gave stands for, `None` for one GtkSourceView has not.
    languages: HashMap<String, Option<sourceview5::Language>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

/// Colour `blocks` of `text`, whose characters `offsets` counts, in `buffer`, a note in `syntax`,
/// and take the colour off code that is no longer a block's.
pub fn apply(
    buffer: &sourceview5::Buffer,
    syntax: Syntax,
    text: &str,
    offsets: &Offsets,
    blocks: &[CodeBlock],
) {
    let caret = buffer.iter_at_mark(&buffer.get_insert()).offset();
    let mut nearest: Vec<&CodeBlock> = blocks.iter().collect();
    nearest.sort_by_key(|b| {
        b.code
            .first()
            .map(|r| (offsets.char_of(r.start) - caret).abs())
    });
    let started = Instant::now();
    let mut later = Vec::new();
    let mut wanted: HashMap<String, Vec<Range<i32>>> = HashMap::new();
    for block in nearest {
        let Some(language) = STATE.with_borrow_mut(|state| state.language(&block.lang)) else {
            continue;
        };
        let code: String = block.code.iter().map(|r| &text[r.clone()]).collect();
        let key = key(&language, &code);
        let runs = STATE.with_borrow_mut(|state| match state.runs.get(&key) {
            Some(runs) => Some(runs.clone()),
            None if started.elapsed() < BUDGET => Some(state.highlight(&language, &code, key)),
            None => None,
        });
        let Some(runs) = runs else {
            later.push((language, code));
            continue;
        };
        // The code's characters as the block's ranges hold them, each with where it starts in
        // the buffer: one range but in a quote, whose `>` markers break it up.
        let mut at = 0;
        let pieces: Vec<(Range<i32>, i32)> = block
            .code
            .iter()
            .map(|r| {
                let chars = offsets.char_of(r.end) - offsets.char_of(r.start);
                at += chars;
                (at - chars..at, offsets.char_of(r.start))
            })
            .collect();
        for (run, name) in runs.iter() {
            for (piece, start) in &pieces {
                let (from, to) = (run.start.max(piece.start), run.end.min(piece.end));
                if from < to {
                    let range = start + from - piece.start..start + to - piece.start;
                    wanted.entry(name.clone()).or_default().push(range);
                }
            }
        }
    }
    let table = buffer.tag_table();
    let tags = STATE.with_borrow(|state| {
        let mut tags = Vec::new();
        for (name, look) in &state.looks {
            let want = wanted.remove(name).unwrap_or_default();
            let tag = match table.lookup(name) {
                Some(tag) => tag,
                None if want.is_empty() => continue,
                None => {
                    let tag = gtk::TextTag::new(Some(name));
                    mirror(look, &tag);
                    table.add(&tag);
                    tag
                }
            };
            tags.push((tag, want));
        }
        tags
    });
    crate::highlight::sync_tags(buffer, tags);
    if !later.is_empty() {
        highlight_later(buffer, syntax, later);
    }
}

/// Highlight `blocks` in idle time, [`BUDGET`] a turn, and style `buffer` again once they all are:
/// one job a buffer, whose last pass sets off another for whatever is still not highlighted.
fn highlight_later(
    buffer: &sourceview5::Buffer,
    syntax: Syntax,
    mut blocks: Vec<(sourceview5::Language, String)>,
) {
    let address = buffer.as_ptr() as usize;
    if !STATE.with_borrow_mut(|state| state.pending.insert(address)) {
        return;
    }
    let buffer = buffer.downgrade();
    glib::idle_add_local(move || {
        let started = Instant::now();
        while started.elapsed() < BUDGET
            && let Some((language, code)) = blocks.pop()
        {
            let key = key(&language, &code);
            STATE.with_borrow_mut(|state| {
                if !state.runs.contains_key(&key) {
                    state.highlight(&language, &code, key);
                }
            });
        }
        if !blocks.is_empty() {
            return glib::ControlFlow::Continue;
        }
        STATE.with_borrow_mut(|state| state.pending.remove(&address));
        if let Some(buffer) = buffer.upgrade() {
            crate::highlight::apply(&buffer, syntax);
        }
        glib::ControlFlow::Break
    });
}

/// Give `buffer`'s code tags the looks of the theme now in force.
pub fn restyle(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    STATE.with_borrow(|state| {
        state.hidden.values().for_each(crate::editor::sync_scheme);
        for (name, look) in &state.looks {
            if let Some(tag) = table.lookup(name) {
                mirror(look, &tag);
            }
        }
    });
}

impl State {
    /// [`language`], asked once a name: guessing from an extension reads content types.
    fn language(&mut self, name: &str) -> Option<sourceview5::Language> {
        let name = name.to_ascii_lowercase();
        self.languages
            .entry(name)
            .or_insert_with_key(|name| language(name))
            .clone()
    }

    /// The runs of `code` in `language`, highlighted now and kept by `key`.
    fn highlight(
        &mut self,
        language: &sourceview5::Language,
        code: &str,
        key: (String, u64),
    ) -> Runs {
        if self.runs.len() >= KEPT {
            self.runs.clear();
        }
        let hidden = self.hidden.entry(key.0.clone()).or_insert_with(|| {
            let buffer = sourceview5::Buffer::with_language(language);
            buffer.set_highlight_matching_brackets(false);
            buffer
        });
        // The theme in force, which may have changed with no note open to restyle.
        crate::editor::sync_scheme(hidden);
        hidden.set_text(code);
        let (start, end) = hidden.bounds();
        hidden.ensure_highlight(&start, &end);
        let mut runs = Vec::new();
        let mut at = start;
        while !at.is_end() {
            let mut next = at;
            next.forward_to_tag_toggle(None::<&gtk::TextTag>);
            // The style tags have no names, the context classes' do; the one in front is seen.
            if let Some(look) = at.tags().into_iter().rev().find(|t| t.name().is_none()) {
                let name = match self.looks.iter().find(|(_, l)| *l == look) {
                    Some((name, _)) => name.clone(),
                    None => {
                        let name = format!("code:{}:{}", key.0, self.looks.len());
                        self.looks.push((name.clone(), look));
                        name
                    }
                };
                runs.push((at.offset()..next.offset(), name));
            }
            at = next;
        }
        let runs = Rc::new(runs);
        self.runs.insert(key, runs.clone());
        runs
    }
}

/// What a block's runs are kept by: its language and a hash of its code.
fn key(language: &sourceview5::Language, code: &str) -> (String, u64) {
    let mut hash = DefaultHasher::new();
    code.hash(&mut hash);
    (language.id().to_string(), hash.finish())
}

/// The GtkSourceView language a block names, as a code tab would colour a file of it: by its id
/// (`rust`, `c`), by a name blocks give it that is no id (`python`, `bash`), or as a file's
/// extension (`py`, `rs`, `ts`). `name` is in lower case.
fn language(name: &str) -> Option<sourceview5::Language> {
    let id = match name {
        "python" => "python3",
        "bash" | "shell" | "zsh" => "sh",
        "javascript" => "js",
        "c++" => "cpp",
        "tex" => "latex",
        "golang" => "go",
        "dockerfile" => "docker",
        _ => name,
    };
    let manager = sourceview5::LanguageManager::default();
    manager
        .language(id)
        .or_else(|| manager.guess_language(Some(format!("block.{name}")), None))
}

/// Give `to` the look of `from`: its colours, weight, slant, underline and strike.
fn mirror(from: &gtk::TextTag, to: &gtk::TextTag) {
    let foreground = from.foreground_rgba().filter(|_| from.is_foreground_set());
    let background = from.background_rgba().filter(|_| from.is_background_set());
    to.set_foreground_rgba(foreground.as_ref());
    to.set_background_rgba(background.as_ref());
    to.set_weight(from.weight());
    to.set_property("weight-set", from.is_weight_set());
    to.set_style(from.style());
    to.set_property("style-set", from.is_style_set());
    to.set_underline(from.underline());
    to.set_property("underline-set", from.is_underline_set());
    to.set_strikethrough(from.is_strikethrough());
    to.set_property("strikethrough-set", from.is_strikethrough_set());
}
