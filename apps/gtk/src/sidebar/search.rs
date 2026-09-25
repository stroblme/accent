//! The Search pane: the box, the toggles, the replace row and the query loop behind them.
//!
//! Search runs off the main loop. [`Data::search`] is called on a worker thread, so a full-vault
//! query never costs a keystroke; a bar pulsing above the results says one is running and the
//! previous results stay on screen until the new ones arrive. A query cannot be called back once
//! it is out, so a newer one simply supersedes it: each carries the number it was asked under,
//! and an answer arriving under a newer number is dropped rather than painted.

use super::{OnOpen, Target};
use crate::dialogs::confirm;
use crate::recall::{self, QUERIES, REPLACEMENTS};
use crate::widgets::{Debounce, Pulse, scroller, status_page};
use accent_core::index::{MIN_INFIX, Match, SearchHit};
use accent_core::path::{basename, parent_dir};
use accent_core::search::{self, Options, Regex};
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, Ref};
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Same value as the palette and the switcher (DESIGN.md, Motion): long enough to swallow a burst
/// of keystrokes, short enough to feel immediate. The only wait between a keystroke and its
/// query: the entry's own `search-delay`, 150 ms by default, is set to nothing.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// One step of the search progress bar. GTK4 has no indeterminate mode, so the bar is stepped by
/// a timer of ours; at the default pulse step this crosses the trough in about two seconds.
const PULSE: Duration = Duration::from_millis(80);
/// How many matches Replace All rewrites without asking first. A rewrite reaches files nobody has
/// open, and past 64 MB of them cannot be undone, which is the choice DESIGN.md's States section
/// keeps an `AdwAlertDialog` for; the one match the pane is already showing struck through is the
/// case where the preview *is* the confirmation.
const CONFIRM_ABOVE: usize = 1;
/// How many pulses a query has to outlive before its bar is drawn at all (DESIGN.md, Loading).
/// Nothing else in the window starts a search, so a query the user did not ask for — the requery
/// a changed file triggers under a finished search — is over inside this and never draws one:
/// on the generated 40k-file vault, 0.8–11 ms ranked and 50–67 ms as the exact scan. The one
/// query there that does outlive it is the reader's own first exact scan on an index the page
/// cache has not got yet, which reads every body off the disk (459 ms) and is worth reporting.
const SHOW_AFTER: u32 = 2;
/// How long a ranked answer stands before its two slower halves are asked for — the files that
/// hold it mid-word, and with All on the walk past the index — counted from when it was asked.
/// That is [`DEBOUNCE`] after the last keystroke, so the two start some 400 ms after the typing
/// stops. The walk took 132 ms on the generated 40k-file vault and a mid-word query up to 140 ms,
/// too slow for every keystroke, and a query typed past in the meantime asks for neither.
const WIDEN_AFTER: Duration = Duration::from_millis(350);
/// The heading over the rows All's walk found past the index.
const NOT_INDEXED: &str = "Not Indexed";
/// One query, already compiled. Built on the main thread from what the search box says, so an
/// invalid pattern is reported without a worker thread being spent on it.
pub enum Query {
    /// Ranked full text: what a plain query with no toggle means, and the fast path. The flag is
    /// the All toggle — search what git ignores and what the walk skipped, as well.
    Fts(String, bool),
    /// Exact matching over bodies, one result row per match. `all` means what it does above.
    ///
    /// The pattern travels as what was typed plus the toggles rather than as the compiled
    /// `Regex`: the vault may be on another machine, and case-insensitivity lives in the builder
    /// rather than in the pattern string, so the string alone would quietly change the search.
    /// It is still compiled here first, so an unusable pattern costs no worker thread.
    Grep {
        text: String,
        options: Options,
        all: bool,
    },
    /// The files the index holds the ranked query in mid-word, leaving out the `skip` files the
    /// ranked rows already list, at most `limit` rows. `all` means what it does above.
    MidWord {
        text: String,
        limit: usize,
        all: bool,
        skip: Vec<String>,
    },
    /// The ranked query's text as a case-insensitive literal over the trees the index never
    /// entered, at most `limit` rows. `stop` turns true once a newer question has been asked, and
    /// the walk ends there.
    Walk {
        text: String,
        limit: usize,
        stop: Box<dyn Fn() -> bool + Send + Sync>,
    },
}

/// What a [`Query`] answered.
pub enum Answer {
    Fts(Vec<SearchHit>),
    Grep {
        /// The matches in the files the index holds a body for, which Replace All rewrites.
        hits: Vec<Match>,
        /// How many of those there are in all, which the capped list cannot say.
        total: usize,
        /// The matches All's walk found past the index. Listed, never rewritten.
        walked: Vec<Match>,
    },
    MidWord(Vec<SearchHit>),
    Walked(Vec<Match>),
}

/// What the Search pane asks of the index.
// Boxed closures returning an answer are the whole point of this struct; a type alias per field
// would only hide the signature the caller has to write.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// Runs on a worker thread, so it may touch nothing the main loop owns.
    pub search: Arc<dyn Fn(Query) -> Answer + Send + Sync>,
    /// Rewrite every match in the vault. `literal` says whether `$1` in the replacement is a
    /// capture group or two characters, and the second flag is the All toggle, which the rewrite
    /// reads the way the count beside the rows did. It writes one file at a time, so it runs off
    /// the main loop and calls `done` there once it has: the pane stays busy until then.
    pub replace_all: Box<dyn Fn(String, Options, String, bool, bool, Box<dyn FnOnce()>)>,
}

/// FTS5 wraps matched terms in `«` and `»` (see `Index::search`). Escape first, so a note holding a
/// literal `<` or `&` cannot corrupt the markup, and only then turn the markers into bold. A note
/// can contain those guillemets itself, so nesting is counted rather than substituted blindly and
/// an unclosed run is closed at the end; the result always parses.
fn snippet_markup(snippet: &str) -> String {
    let escaped = glib::markup_escape_text(snippet);
    let mut out = String::with_capacity(escaped.len() + 7);
    let mut depth = 0usize;
    for c in escaped.chars() {
        match c {
            '«' => {
                if depth == 0 {
                    out.push_str("<b>");
                }
                depth += 1;
            }
            '»' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push_str("</b>");
                }
            }
            // Runs of whitespace collapse to one space, and that includes the newlines an FTS
            // snippet carries out of the note body. Pango counts `lines(2)` per paragraph, so a
            // snippet spanning a frontmatter block rendered a dozen lines and one hit filled the
            // pane; as a single paragraph the row ellipsizes after two lines as intended.
            c if c.is_whitespace() => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    if depth > 0 {
        out.push_str("</b>");
    }
    out.truncate(out.trim_end().len());
    out
}

/// A grep row's snippet: the matched line with the match in bold, or, once the replace row is
/// open, the match struck through beside what it would become. `accent` is the only colour this
/// module names and it comes from the style manager (DESIGN.md, Colour).
fn match_markup(line: &str, range: Range<usize>, replaced: Option<&str>, accent: &str) -> String {
    let esc = |s: &str| glib::markup_escape_text(s).to_string();
    let (before, matched, after) = (
        &line[..range.start],
        &line[range.clone()],
        &line[range.end..],
    );
    match replaced {
        None => format!("{}<b>{}</b>{}", esc(before), esc(matched), esc(after)),
        Some(new) => format!(
            "{}<s>{}</s> <span foreground=\"{accent}\">{}</span>{}",
            esc(before),
            esc(matched),
            esc(new),
            esc(after)
        ),
    }
}

/// A row's dim half: the folder it sits in, with the trailing slash that says it is one, and the
/// line the match is on where the row is one match — "notes/deep/ — line 12". A row names the
/// file — `design.md` and the folder holding it — rather than the note's title: the title hides
/// the extension, and two notes titled the same are then one row twice.
fn dir_label(rel_path: &str, line: Option<u32>) -> String {
    let dir = match parent_dir(rel_path) {
        "" => String::new(),
        dir => format!("{dir}/"),
    };
    match (line, dir.is_empty()) {
        (None, _) => dir,
        (Some(line), true) => format!("line {line}"),
        (Some(line), false) => format!("{dir} — line {line}"),
    }
}

/// The row under a file whose matches the per-file cap cut short. The dim line alone, and it
/// opens the file where that file's first listed match is.
fn more_row(rel_path: &str, more: usize, first: Option<Range<usize>>) -> Row {
    Row {
        rel_path: rel_path.to_string(),
        at: first,
        name: String::new(),
        dir: format!("+{more} more in this file"),
        snippet: String::new(),
    }
}

/// What an answer holds, where the progress bar is drawn between queries: "12 results in 3
/// files". A number the list's cap may have cut short is a floor, and says so with a `+`.
fn count_label(found: usize, files: usize, more_found: bool, more_files: bool) -> String {
    let counted = |n: usize, more: bool, noun: &str| match (n, more) {
        (1, false) => format!("1 {noun}"),
        (n, false) => format!("{n} {noun}s"),
        (n, true) => format!("{n}+ {noun}s"),
    };
    format!(
        "{} in {}",
        counted(found, more_found, "result"),
        counted(files, more_files, "file")
    )
}

/// The system accent as pango markup understands it. `Widget::color()` and the style manager are
/// the only colour sources in the app, and pango's parser takes `#rrggbb` and colour names only,
/// so the resolved accent is spelled out here rather than handed over as a CSS function.
fn accent_markup_colour() -> String {
    let c = adw::StyleManager::default().accent_color_rgba();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(c.red()),
        byte(c.green()),
        byte(c.blue())
    )
}

/// A finished result row. Both query kinds meet here, already marked up, so binding a row costs
/// nothing and the factory does not have to know which kind produced it.
struct Row {
    /// Empty on a heading, which names the rows under it and opens nothing.
    rel_path: String,
    /// The match's byte range in the note; `None` opens the note at the top, which is all a hit
    /// on a note's title alone can name.
    at: Option<Range<usize>>,
    /// The file's name, and beside it in dim the folder it sits in — plus, on a grep row, the
    /// line. A row with no `snippet` is a tail row: the dim line alone, saying what the per-file
    /// cap left out.
    name: String,
    dir: String,
    snippet: String,
}

// --- the query loop --------------------------------------------------------------------------------

/// What the search box is asking for. Compared instead of the compiled [`Query`], because `Regex`
/// has no equality and two searches are the same search when the same text and toggles made them.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    text: String,
    options: Options,
    /// Exact matching rather than ranked full text: any of the three query toggles on, or the
    /// replace row open.
    grep: bool,
    /// Search what is normally left out: what git ignores, and the trees the walk never entered.
    all: bool,
}

/// The search pane's query loop and the widgets it drives, in one `Rc` so the future that waits
/// on a worker thread can hold all of it without cloning a dozen handles.
struct Search {
    data: Rc<Data>,
    entry: gtk::SearchEntry,
    toggles: [gtk::ToggleButton; 4],
    replace_row: gtk::Revealer,
    replace_entry: gtk::Entry,
    apply: gtk::Button,
    progress: gtk::ProgressBar,
    /// The timer stepping [`Search::progress`] while a query is on a worker thread.
    pulse: Pulse,
    /// What the rows on screen add up to ([`count_label`]), in the bar's place while it is not
    /// drawn.
    count: gtk::Label,
    body: gtk::Stack,
    results: gio::ListStore,
    /// Which question the rows on screen are meant to answer. Every query takes the next number
    /// before it starts, so an answer arriving under a newer one is dropped instead of painted:
    /// a slow first query — a remote vault, or an All walk — used to hold every keystroke typed
    /// after it until it landed. Atomic, because a walk reads it from its own threads to stop.
    generation: Arc<AtomicU64>,
    /// How many queries are on worker threads. A query cannot be called back, so the count comes
    /// down as each one lands, whether its answer was wanted or not.
    running: Cell<usize>,
    /// A Replace All is rewriting the vault. It writes the very files a query would read, so the
    /// pane asks nothing else until it is over.
    replacing: Cell<bool>,
    /// What the button last promised to rewrite, which is what the confirmation says out loud.
    total: Cell<usize>,
    /// What the ranked rows on screen counted — results, files, and whether the cap cut them —
    /// which the rows a walk appends later are added to.
    counted: Cell<(usize, usize, bool)>,
}

impl Search {
    fn key(&self) -> Key {
        let options = Options {
            case: self.toggles[0].is_active(),
            word: self.toggles[1].is_active(),
            regex: self.toggles[2].is_active(),
        };
        Key {
            text: self.entry.text().to_string(),
            options,
            // Replacing is an exact operation, so opening the replace row switches modes too:
            // a ranked full-text hit is not a place in a file that can be rewritten.
            //
            // All still does not switch modes, and that is a decision rather than an oversight:
            // forcing exact would turn a two-word ranked query into a literal-substring one, so
            // asking for more files would quietly find fewer, and it would pay `grep_unindexed`'s
            // walk on every keystroke. What it costs is that the walked trees stay out of ranked
            // results, which is what All's tooltip now says out loud.
            grep: options.any() || self.replace_row.reveals_child(),
            all: self.toggles[3].is_active(),
        }
    }

    /// What Replace All would write, or `None` while the replace row is closed.
    fn replacement(&self) -> Option<String> {
        self.replace_row
            .reveals_child()
            .then(|| self.replace_entry.text().to_string())
    }

    /// Run the pulse timer while a query is on a worker thread, and show the bar once that query
    /// has run long enough to be worth reporting.
    ///
    /// Opacity rather than visibility: the bar keeps its height either way, so results do not jump
    /// down a few pixels the moment a query starts. And it is shown late rather than at once,
    /// because a bar that appears and goes in the same breath reads as a flash, not as progress.
    fn set_busy(&self, busy: bool) {
        match busy {
            // A pulse already running is left where it is: restarting it on every keystroke —
            // which an empty or unparseable one used to do — blinked the bar off for a step in
            // the middle of a query that was still going.
            true => self.pulse.start(PULSE, SHOW_AFTER),
            false => {
                self.pulse.stop();
                self.progress.set_opacity(0.0);
            }
        }
    }

    /// Whether anything of this pane's is still on a worker thread.
    fn busy(&self) -> bool {
        self.running.get() > 0 || self.replacing.get()
    }

    /// Run what the box currently asks for. Whatever was already running is superseded rather
    /// than waited for: its answer is dropped when it lands.
    fn start(self: &Rc<Self>) {
        let key = self.key();
        // Taken before anything else, so the branches that paint without asking the vault also
        // put the answer of a query still in flight out of date.
        let mine = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        // The bar tracks what is running in every branch: a query still on a worker thread keeps
        // it pulsing, and the one that lands after the box was cleared takes it down through here.
        if key.text.trim().is_empty() {
            self.entry.remove_css_class("error");
            self.results.remove_all();
            self.body.set_visible_child_name("prompt");
            self.set_busy(self.busy());
            self.set_total(0);
            self.count.set_text("");
            return;
        }
        let query = match compile(&key) {
            Ok(query) => query,
            // Only regex mode can fail to compile, and the message is always "that is not a
            // pattern", so the entry says it in place rather than through a toast.
            Err(e) => {
                tracing::debug!("invalid search pattern {:?}: {e}", key.text);
                self.entry.add_css_class("error");
                self.body.set_visible_child_name("invalid");
                self.set_busy(self.busy());
                self.set_total(0);
                self.count.set_text("");
                return;
            }
        };
        self.entry.remove_css_class("error");
        // A rewrite is the one thing worth waiting for: it is writing the files the query reads.
        if self.replacing.get() {
            return;
        }
        self.running.set(self.running.get() + 1);
        self.set_busy(true);
        self.apply.set_sensitive(false);

        let search = self.clone();
        glib::spawn_future_local(async move {
            let run = search.data.search.clone();
            let t0 = Instant::now();
            let answer = crate::work::off_thread("search", move || run(query)).await;
            search.running.set(search.running.get() - 1);
            let Some(answer) = answer else {
                return search.set_busy(search.busy());
            };
            tracing::debug!(
                query = key.text,
                grep = key.grep,
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "sidebar query"
            );
            // What the ranked rows list, which the mid-word rows below them must not repeat. A
            // query shorter than a trigram has none, and one that filled the list has no room.
            let listed = match &answer {
                Answer::Fts(hits) => Some(hits.len()),
                _ => None,
            };
            let skip = match &answer {
                Answer::Fts(hits)
                    if !hits.is_empty()
                        && hits.len() < crate::SEARCH_LIMIT
                        && key.text.trim().chars().count() >= MIN_INFIX =>
                {
                    // A file's rows arrive together, so its repeats are neighbours.
                    let mut files: Vec<String> = hits.iter().map(|h| h.rel_path.clone()).collect();
                    files.dedup();
                    Some(files)
                }
                _ => None,
            };
            // The box has moved on since this was asked, so a newer query is already on its way
            // with the answer that belongs on screen. Old results stay up until it lands.
            if search.generation.load(Ordering::Relaxed) == mine {
                search.show(&key, answer);
                if let Some(listed) = listed
                    && (skip.is_some() || key.all)
                {
                    search.widen_soon(key, mine, t0, listed, skip);
                }
            }
            search.set_busy(search.busy());
        });
    }

    /// Add the slower halves of a ranked answer once it has stood for [`WIDEN_AFTER`]: the files
    /// that hold the query mid-word, in the order the trigram index gives them, then, with All
    /// on, what the walk finds past the index under [`NOT_INDEXED`]. Each goes below what is
    /// already listed, in that order, into whatever room the rows above left.
    ///
    /// Neither can be ranked against the prefix rows — a trigram match has no term statistics
    /// and a walked file has no FTS row — so both come after them. A newer question cancels all
    /// of it: the wait ends without asking, a walk already running reads the generation before
    /// each file and stops, and an answer that lands anyway is dropped, so it never shows under
    /// a query asked after it.
    fn widen_soon(
        self: &Rc<Self>,
        key: Key,
        mine: u64,
        asked: Instant,
        mut listed: usize,
        skip: Option<Vec<String>>,
    ) {
        let search = self.clone();
        let text = key.text.trim().to_string();
        glib::spawn_future_local(async move {
            glib::timeout_future(WIDEN_AFTER.saturating_sub(asked.elapsed())).await;
            let current = search.generation.clone();
            let stale = move || current.load(Ordering::Relaxed) != mine;
            if let Some(skip) = skip
                && !stale()
            {
                let room = crate::SEARCH_LIMIT - listed;
                let query = Query::MidWord {
                    text: text.clone(),
                    limit: room,
                    all: key.all,
                    skip,
                };
                if let Some(Answer::MidWord(hits)) = search.ask("mid_word", &text, query).await
                    && !stale()
                {
                    listed += hits.len();
                    search.show_mid_word(hits, room);
                }
            }
            let room = crate::SEARCH_LIMIT.saturating_sub(listed);
            if !key.all || room == 0 || stale() {
                return;
            }
            let query = Query::Walk {
                text: text.clone(),
                limit: room,
                stop: Box::new(stale.clone()),
            };
            if let Some(Answer::Walked(walked)) = search.ask("walk", &text, query).await
                && !stale()
            {
                search.show_walked(&text, walked, room);
            }
        });
    }

    /// Run one of [`widen_soon`](Self::widen_soon)'s questions on a worker thread, the bar
    /// pulsing meanwhile.
    async fn ask(self: &Rc<Self>, pass: &'static str, text: &str, query: Query) -> Option<Answer> {
        self.running.set(self.running.get() + 1);
        self.set_busy(true);
        let run = self.data.search.clone();
        let t0 = Instant::now();
        let answer = crate::work::off_thread(pass, move || run(query)).await;
        self.running.set(self.running.get() - 1);
        self.set_busy(self.busy());
        tracing::debug!(
            query = text,
            pass,
            ms = t0.elapsed().as_secs_f64() * 1e3,
            "sidebar pass"
        );
        answer
    }

    /// Append the mid-word rows below the ranked ones, and count them in. Rows that filled the
    /// `room` they were given may have left matches out, so the count becomes a floor.
    fn show_mid_word(&self, hits: Vec<SearchHit>, room: usize) {
        if hits.is_empty() {
            return;
        }
        let (found, files, cut) = self.counted.get();
        let found = found + hits.iter().map(|h| 1 + h.more).sum::<usize>();
        let new_files = hits.iter().map(|h| h.rel_path.as_str());
        let files = files + new_files.collect::<HashSet<_>>().len();
        let cut = cut || hits.len() >= room;
        self.counted.set((found, files, cut));
        self.count.set_text(&count_label(found, files, cut, cut));
        let objects: Vec<glib::BoxedAnyObject> = fts_rows(hits)
            .into_iter()
            .map(glib::BoxedAnyObject::new)
            .collect();
        self.results.splice(self.results.n_items(), 0, &objects);
    }

    /// Append the walk's rows below the ranked ones, and count them in. A walk that filled the
    /// `room` it was given may have left matches out, so its count is a floor.
    fn show_walked(&self, text: &str, walked: Vec<Match>, room: usize) {
        let Ok(re) = search::pattern(text, Options::default()) else {
            return;
        };
        if walked.is_empty() {
            return;
        }
        let (found, files, cut) = self.counted.get();
        let found = found + walked.iter().map(|m| 1 + m.more).sum::<usize>();
        let walked_files = walked.iter().map(|m| m.rel_path.as_str());
        let files = files + walked_files.collect::<HashSet<_>>().len();
        let cut = cut || walked.len() >= room;
        self.count.set_text(&count_label(found, files, cut, cut));
        let rows = walked_rows(walked, &re, &accent_markup_colour());
        self.body.set_visible_child_name("results");
        let objects: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.results.splice(self.results.n_items(), 0, &objects);
    }

    fn show(&self, key: &Key, answer: Answer) {
        // A row is one match, and a file's last row counts the ones the per-file cap left off it.
        // A list as long as the cap may have been cut short, and cannot count what it left out.
        let (rows, count) = match answer {
            Answer::Fts(hits) => {
                self.set_total(0);
                let found = hits.iter().map(|h| 1 + h.more).sum();
                let files = hits.iter().map(|h| h.rel_path.as_str());
                let files = files.collect::<HashSet<_>>().len();
                let cut = hits.len() >= crate::SEARCH_LIMIT;
                self.counted.set((found, files, cut));
                (fts_rows(hits), count_label(found, files, cut, cut))
            }
            Answer::Grep {
                hits,
                total,
                walked,
            } => {
                self.set_total(total);
                let Ok(re) = compile_regex(key) else {
                    return;
                };
                // `total` is every match the index holds, however many were listed; the walk
                // past it counts what it listed alone, so only All's number can be a floor.
                let found = total + walked.iter().map(|m| 1 + m.more).sum::<usize>();
                let files = hits.iter().chain(&walked).map(|m| m.rel_path.as_str());
                let files = files.collect::<HashSet<_>>().len();
                let cut = hits.len() + walked.len() >= crate::SEARCH_LIMIT;
                let count = count_label(found, files, cut && key.all, cut);
                let replacement = self.replacement();
                let accent = accent_markup_colour();
                let literal = !key.options.regex;
                let mut rows = grep_rows(hits, &re, replacement.as_deref(), literal, &accent);
                // No preview on a walked row: the rewrite never opens its file, so striking the
                // match through would promise an edit that does not happen.
                rows.extend(walked_rows(walked, &re, &accent));
                (rows, count)
            }
            // Appended by `show_mid_word` and `show_walked`, never shown on their own.
            Answer::MidWord(_) | Answer::Walked(_) => return,
        };
        // No Results says it already.
        self.count
            .set_text(if rows.is_empty() { "" } else { &count });
        self.body
            .set_visible_child_name(if rows.is_empty() { "empty" } else { "results" });
        let objects: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.results.splice(0, self.results.n_items(), &objects);
    }

    /// How many matches the button would rewrite — not how many rows there are. The list is
    /// capped and, with All on, reaches past the index; the number is uncapped and counts the
    /// matches in the files the index holds a body for, because those are what Replace All opens.
    fn set_total(&self, total: usize) {
        self.total.set(total);
        self.apply.set_label(&format!("Replace All ({total})"));
        self.apply.set_sensitive(total > 0);
    }

    /// Rewrite the vault, then ask the same question again so the rows show what is there now.
    ///
    /// The rewrite is one fsync per note and runs on a worker thread; 245 notes took 1.9 s on the
    /// generated vault and 3.3k took 35 s, all of which the main loop used to spend frozen. The
    /// pane marks itself busy for the duration instead — the bar pulses, Replace All goes
    /// insensitive, and a query typed meanwhile waits for the writes rather than racing them.
    fn replace_all(self: &Rc<Self>) {
        // Compiled and thrown away: the vault is asked in the same terms the box holds, and this
        // is only here to refuse a pattern that does not compile before anything is asked.
        let (Ok(_), Some(_)) = (compile_regex(&self.key()), self.replacement()) else {
            return;
        };
        if self.busy() {
            return;
        }
        let total = self.total.get();
        if total > CONFIRM_ABOVE {
            let search = self.clone();
            return confirm(
                &self.apply,
                &format!("Replace {total} Matches?"),
                "Every match is rewritten in the file it is in. A rewrite of more than 64 MB \
                 cannot be undone.",
                "Replace All",
                true,
                move || search.run_replace_all(),
            );
        }
        self.run_replace_all();
    }

    /// The rewrite itself, once it has been asked for and, past [`CONFIRM_ABOVE`], agreed to.
    fn run_replace_all(self: &Rc<Self>) {
        let key = self.key();
        let (Ok(_), Some(replacement)) = (compile_regex(&key), self.replacement()) else {
            return;
        };
        if self.busy() {
            return;
        }
        recall::remember(&QUERIES, &key.text);
        recall::remember(&REPLACEMENTS, &replacement);
        self.replacing.set(true);
        self.set_busy(true);
        self.apply.set_sensitive(false);
        let search = self.clone();
        (self.data.replace_all)(
            key.text.clone(),
            key.options,
            replacement,
            !key.options.regex,
            key.all,
            Box::new(move || {
                search.replacing.set(false);
                search.start();
            }),
        );
    }
}

/// A [`Key`] as the worker thread needs it.
fn compile(key: &Key) -> Result<Query, search::Error> {
    match key.grep {
        true => {
            compile_regex(key)?;
            Ok(Query::Grep {
                text: key.text.clone(),
                options: key.options,
                all: key.all,
            })
        }
        false => Ok(Query::Fts(key.text.clone(), key.all)),
    }
}

fn compile_regex(key: &Key) -> Result<Regex, search::Error> {
    search::pattern(&key.text, key.options)
}

/// One row per hit of the ranked search, which is one per occurrence: the line the match sits on
/// with the match marked in it, the file and that line as the dim half, and the same tail row a
/// grep row gets where the per-file cap cut a file short. A hit whose body does not hold the
/// query — a note found by its title — carries no line and quotes the head of the note instead.
fn fts_rows(hits: Vec<SearchHit>) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(hits.len());
    // A file's hits arrive together, so the row under a new path is that file's first — which is
    // where its tail row opens it.
    let mut first: Option<(String, Option<Range<usize>>)> = None;
    for hit in hits {
        if first.as_ref().is_none_or(|(rel, _)| *rel != hit.rel_path) {
            first = Some((hit.rel_path.clone(), hit.at.clone()));
        }
        rows.push(Row {
            name: basename(&hit.rel_path).to_string(),
            dir: dir_label(&hit.rel_path, hit.line),
            snippet: snippet_markup(&hit.snippet),
            at: hit.at,
            rel_path: hit.rel_path.clone(),
        });
        if hit.more > 0 {
            rows.push(more_row(
                &hit.rel_path,
                hit.more,
                first.as_ref().and_then(|(_, at)| at.clone()),
            ));
        }
    }
    rows
}

/// One row per match, with the diff against the replacement when there is one, and a tail row
/// under a file whose matches the per-file cap cut short.
///
/// ponytail: the replacement is computed by running `re` over the matched text again, which is
/// what makes `$1` expand in the preview. A pattern whose groups depend on context outside the
/// match would preview wrongly; the regex crate has no lookaround, so today that cannot happen.
fn grep_rows(
    hits: Vec<Match>,
    re: &Regex,
    replacement: Option<&str>,
    literal: bool,
    accent: &str,
) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::with_capacity(hits.len());
    // A file's matches arrive together, so the row under a new path is that file's first match —
    // which is where its tail row opens it.
    let mut first: Option<(String, Range<usize>)> = None;
    for m in hits {
        let matched = &m.line_text[m.range.clone()];
        let at = m.offset..m.offset + m.range.len();
        let replaced = replacement.map(|r| match literal {
            true => re.replace(matched, search::NoExpand(r)).into_owned(),
            false => re.replace(matched, r).into_owned(),
        });
        if first.as_ref().is_none_or(|(rel, _)| *rel != m.rel_path) {
            first = Some((m.rel_path.clone(), at.clone()));
        }
        rows.push(Row {
            name: basename(&m.rel_path).to_string(),
            dir: dir_label(&m.rel_path, Some(m.line)),
            snippet: match_markup(&m.line_text, m.range, replaced.as_deref(), accent),
            at: Some(at),
            rel_path: m.rel_path.clone(),
        });
        if m.more > 0 {
            rows.push(more_row(
                &m.rel_path,
                m.more,
                first.as_ref().map(|(_, at)| at.clone()),
            ));
        }
    }
    rows
}

/// The rows All's walk found past the index, under a heading that says so: the matched line
/// marked as a grep row's is, and no replacement preview, because Replace All never opens them.
fn walked_rows(walked: Vec<Match>, re: &Regex, accent: &str) -> Vec<Row> {
    if walked.is_empty() {
        return Vec::new();
    }
    let heading = Row {
        rel_path: String::new(),
        at: None,
        name: NOT_INDEXED.to_string(),
        dir: String::new(),
        snippet: String::new(),
    };
    let mut rows = vec![heading];
    rows.extend(grep_rows(walked, re, None, true, accent));
    rows
}

pub(super) struct Pane {
    pub(super) widget: gtk::Widget,
    pub(super) entry: gtk::SearchEntry,
    pub(super) replace_toggle: gtk::ToggleButton,
    pub(super) all_toggle: gtk::ToggleButton,
    pub(super) replace_entry: gtk::Entry,
    /// Ask the current question again. The window calls it when the ignore set changes under a
    /// query that is already on screen.
    pub(super) restart: Rc<dyn Fn()>,
    /// The vault moved while this pane was behind the switcher, so its rows no longer answer the
    /// question. Set by [`Sidebar::requery_search_soon`](super::Sidebar::requery_search_soon),
    /// cleared by the query the next show runs. It starts false, unlike the Tags pane's: a box
    /// with nothing in it has nothing to catch up on.
    pub(super) dirty: Rc<Cell<bool>>,
    /// The Replace All button, and which page the body is showing with the rows on it — each by
    /// its name, or a tail row by its dim line — and what the count says. What
    /// `ACCENT_BENCH_REPLACE` presses and reads: nothing else can say whether the rows left
    /// standing after a rewrite are the new text's.
    pub(super) apply: gtk::Button,
    pub(super) state: Rc<dyn Fn() -> (String, Vec<String>, String)>,
}

pub(super) fn pane(data: &Rc<Data>, on_open: &OnOpen) -> Pane {
    // ponytail: rows are `glib::BoxedAnyObject`s wrapping a `Row` instead of a GObject with typed
    // properties, the same trade `tree.rs` documents. Define a real item type if the row ever
    // needs bindable state.
    let results = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = crate::widgets::factory(
        |_| {
            let name = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            name.add_css_class("heading");
            // The folder shares the name's line and gives way first, cut at its front: the last
            // folders are what tell two `design.md`s apart, and a grep row's line number sits here
            // too, so both survive the cut.
            let dir = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::Start)
                .build();
            dir.add_css_class("dim-label");
            let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            head.append(&gtk::Image::new());
            head.append(&name);
            head.append(&dir);
            let snippet = gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .lines(2)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            snippet.add_css_class("dim-label");
            // `.navigation-sidebar` gives its rows horizontal padding only, so a two-line row sits on
            // the top and bottom edges of its own selection pill. 6 is the scale's inside-a-group step
            // (DESIGN.md, Spacing).
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            row.set_margin_top(6);
            row.set_margin_bottom(6);
            row.append(&head);
            row.append(&snippet);
            row
        },
        |row: &gtk::Box, item| {
            let (Some(head), Some(snippet)) = (
                row.first_child().and_downcast::<gtk::Box>(),
                row.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let Some(icon) = head.first_child().and_downcast::<gtk::Image>() else {
                return;
            };
            let (Some(name), Some(dir)) = (
                icon.next_sibling().and_downcast::<gtk::Label>(),
                head.last_child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let hit: Ref<Row> = boxed.borrow();
            // A heading is small and dim, like the branch popover's Remote, and nothing to open.
            // A recycled row may have been one, which is why every other row sets it back.
            let heading = hit.rel_path.is_empty();
            item.set_activatable(!heading);
            item.set_selectable(!heading);
            name.set_css_classes(match heading {
                true => &["caption-heading", "dim-label"],
                false => &["heading"],
            });
            // A tail row has no name, and no icon either: it continues the file above it.
            icon.set_icon_name(Some(crate::doc::icon_for(&hit.rel_path)));
            icon.set_visible(!hit.name.is_empty() && !heading);
            name.set_text(&hit.name);
            dir.set_text(&hit.dir);
            // A tail row is the dim line alone, so the empty second line is taken away rather than
            // left as a gap under it.
            snippet.set_markup(&hit.snippet);
            snippet.set_visible(!hit.snippet.is_empty());
        },
    );

    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(results.clone()))),
        Some(factory),
    );
    view.add_css_class("navigation-sidebar");
    // One click opens, the rule `tree.rs` and the Git pane already follow: a result is a place to
    // go, and the tab it opens is this pane's preview, so walking the list replaces one tab
    // rather than leaving twenty behind.
    view.set_single_click_activate(true);
    view.connect_activate({
        let on_open = on_open.clone();
        move |view, pos| {
            if let Some(boxed) = view
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<glib::BoxedAnyObject>()
            {
                let row = boxed.borrow::<Row>();
                on_open(&row.rel_path, row.at.clone().map(Target::Range));
            }
        }
    });

    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "Search Files",
            "Type to search this vault. Ignored files are left out; All puts them back.",
        ),
        Some("prompt"),
    );
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "No Results",
            "Try a different search.",
        ),
        Some("empty"),
    );
    body.add_named(
        &status_page(
            "dialog-warning-symbolic",
            "Invalid Pattern",
            "This is not a valid regular expression.",
        ),
        Some("invalid"),
    );
    body.add_named(&scroller(&view), Some("results"));
    body.set_visible_child_name("prompt");

    // No `search-delay`: [`DEBOUNCE`] is the pane's one wait, and the entry's 150 ms default in
    // front of it made every query start some 200 ms after the keystroke that asked for it.
    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search…")
        .hexpand(true)
        .search_delay(0)
        .build();
    // A bar spanning the width right above the results, not a spinner beside the entry: the wait
    // belongs to the list that is about to change, and the entry needs the whole sidebar width.
    // It is faded rather than hidden, so the results never shift when a query starts.
    let progress = gtk::ProgressBar::builder()
        .opacity(0.0)
        .valign(gtk::Align::Center)
        .build();
    // Between queries its place says what the answer holds, in the status bar's caption. One slot
    // of one height for both, and the bar takes it for exactly as long as it is drawn, which is
    // [`Pulse`]'s to decide: a query over inside the grace period leaves the count where it was.
    let count = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    for class in ["caption", "dim-label", "numeric"] {
        count.add_css_class(class);
    }
    let slot = gtk::Stack::new();
    slot.add_named(&count, Some("count"));
    slot.add_named(&progress, Some("bar"));
    progress.connect_opacity_notify(glib::clone!(
        #[weak]
        slot,
        move |bar| slot.set_visible_child_name(match bar.opacity() > 0.0 {
            true => "bar",
            false => "count",
        })
    ));

    // `edit-find-replace-symbolic`, not a chevron: it names what the button reveals rather than
    // which way a panel opens, and the chevron was invisible for the user who reported this. An
    // icon theme may replace any Adwaita name with artwork of its own, and WhiteSur's
    // `pan-down-symbolic` is written with single-quoted attributes, which GTK4's symbolic
    // recolouring does not parse: the button drew nothing at all (DESIGN.md, Iconography).
    let replace_toggle = gtk::ToggleButton::builder()
        .icon_name("edit-find-replace-symbolic")
        .tooltip_text("Toggle Replace")
        .valign(gtk::Align::Center)
        .build();
    replace_toggle.add_css_class("flat");

    // Text buttons, not icons: Adwaita has no glyph for any of the four, and VS Code's `Aa`,
    // `Word` and `.*` are what a user arriving from there already reads.
    //
    // All's tooltip says which half of "everywhere" it reaches, because the two halves are not
    // the same mechanism: dropping the git-ignored exclusion is a column the ranked search reads
    // too, while the skipped trees are a walk, whose rows nothing can rank and which are listed
    // last under their own heading.
    let toggles = [
        ("Aa", "Match Case"),
        ("Word", "Match Whole Word"),
        (".*", "Use Regular Expression"),
        (
            "All",
            "Search Ignored Files, and List Skipped Folders Under Not Indexed",
        ),
    ]
    .map(|(label, tooltip)| {
        let button = gtk::ToggleButton::builder()
            .label(label)
            .tooltip_text(tooltip)
            .build();
        button.add_css_class("flat");
        button
    });
    let options = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    options.add_css_class("linked");
    options.set_halign(gtk::Align::Start);
    options.set_hexpand(true);
    for button in &toggles {
        options.append(button);
    }
    // The replace toggle shares the row but not the `.linked` group: the three toggles change what
    // the query means, this one reveals another control, and a fourth button welded to them would
    // read as a fourth query option.
    let toggle_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toggle_row.append(&options);
    toggle_row.append(&replace_toggle);

    let replace_entry = gtk::Entry::builder()
        .placeholder_text("Replace…")
        .hexpand(true)
        .build();
    recall::attach(&entry, &QUERIES);
    recall::attach(&replace_entry, &REPLACEMENTS);
    // The box searches as it is typed in, so a query is taken as used once Return is pressed on
    // it or a result it found is opened, rather than at every pause in the typing.
    let used = |entry: &gtk::SearchEntry| recall::remember(&QUERIES, &entry.text());
    entry.connect_activate(used);
    view.connect_activate(glib::clone!(
        #[weak]
        entry,
        move |_, _| used(&entry)
    ));
    let apply = gtk::Button::builder()
        .label("Replace All (0)")
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();
    apply.add_css_class("suggested-action");
    // Stacked rather than side by side: the sidebar's floor is 200 px, where an entry and a button
    // on one line leave neither of them readable.
    let replace_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    replace_box.append(&replace_entry);
    replace_box.append(&apply);
    let replace_row = gtk::Revealer::builder().child(&replace_box).build();

    let search = Rc::new(Search {
        data: data.clone(),
        entry: entry.clone(),
        toggles: toggles.clone(),
        replace_row: replace_row.clone(),
        replace_entry: replace_entry.clone(),
        apply: apply.clone(),
        progress: progress.clone(),
        pulse: Pulse::new(&progress),
        count,
        body: body.clone(),
        results,
        generation: Arc::new(AtomicU64::new(0)),
        running: Cell::new(0),
        replacing: Cell::new(false),
        total: Cell::new(0),
        counted: Cell::new((0, 0, false)),
    });

    // Every handler below holds `search` weakly. Each is connected to a widget `Search` holds, so
    // a strong one is a cycle that outlives the window, and the vault behind `data` with it. The
    // pane's `restart` is the one strong handle, and the sidebar keeps it.
    //
    // A toggle is a click rather than a burst, so only what is typed is debounced.
    let debounce = Rc::new(Debounce::new(DEBOUNCE));
    let debounced: Rc<dyn Fn()> = Rc::new({
        let debounce = debounce.clone();
        glib::clone!(
            #[weak]
            search,
            move || debounce.call(glib::clone!(
                #[weak]
                search,
                move || search.start()
            ))
        )
    });

    entry.connect_search_changed({
        let (debounced, debounce) = (debounced.clone(), debounce.clone());
        glib::clone!(
            #[weak]
            search,
            move |entry| {
                // Clearing the entry is free, so it cancels the pending query and repaints at once.
                if entry.text().trim().is_empty() {
                    debounce.cancel();
                    return search.start();
                }
                debounced();
            }
        )
    });
    // ponytail: a changed replacement re-runs the whole query, because the rows carry finished
    // markup rather than the matches they were built from. Off the main thread it costs nothing
    // the user can feel; cache the last answer if a huge vault ever makes it visible.
    replace_entry.connect_changed({
        let debounced = debounced.clone();
        move |_| debounced()
    });
    for button in &toggles {
        button.connect_toggled(glib::clone!(
            #[weak]
            search,
            move |_| search.start()
        ));
    }
    replace_toggle.connect_toggled({
        let replace_row = replace_row.clone();
        glib::clone!(
            #[weak]
            search,
            move |toggle| {
                replace_row.set_reveal_child(toggle.is_active());
                search.start();
            }
        )
    });
    apply.connect_clicked(glib::clone!(
        #[weak]
        search,
        move |_| search.replace_all()
    ));

    let controls = gtk::Box::new(gtk::Orientation::Vertical, 6);
    controls.set_margin_top(6);
    controls.set_margin_bottom(6);
    controls.set_margin_start(6);
    controls.set_margin_end(6);
    controls.append(&entry);
    controls.append(&toggle_row);
    controls.append(&replace_row);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&controls);
    column.append(&slot);
    column.append(&body);
    Pane {
        widget: column.upcast(),
        entry,
        replace_toggle,
        all_toggle: toggles[3].clone(),
        replace_entry,
        restart: Rc::new({
            let search = search.clone();
            move || search.start()
        }),
        dirty: Rc::new(Cell::new(false)),
        apply,
        state: Rc::new({
            let search = search.clone();
            move || {
                (
                    search
                        .body
                        .visible_child_name()
                        .map(|name| name.to_string())
                        .unwrap_or_default(),
                    (0..search.results.n_items())
                        .filter_map(|i| {
                            search
                                .results
                                .item(i)
                                .and_downcast::<glib::BoxedAnyObject>()
                        })
                        .map(|boxed| {
                            let row = boxed.borrow::<Row>();
                            match row.name.is_empty() {
                                true => row.dir.clone(),
                                false => row.name.clone(),
                            }
                        })
                        .collect(),
                    search.count.text().to_string(),
                )
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_shorter_than_the_grace_period_never_draws_a_bar() {
        // A requery nobody asked for is a few milliseconds on a warm index ranked and some 60 ms
        // as an exact scan; the wait has to be long enough to cover one and short enough that a
        // real query still reports itself.
        assert!((100..=300).contains(&(PULSE * SHOW_AFTER).as_millis()));
    }

    #[test]
    fn snippet_escapes_before_marking_up() {
        // `<` inside the note must survive as text, not as the start of a tag.
        let out = snippet_markup("a «b» < c & d");
        assert_eq!(out, "a <b>b</b> &lt; c &amp; d");
        assert!(pango::parse_markup(&out, '\u{0}').is_ok());
    }

    #[test]
    fn a_snippet_is_one_paragraph_however_the_note_was_wrapped() {
        // `lines(2)` on the row label is counted per paragraph, so a newline here is a row that
        // grows without limit.
        let out = snippet_markup("---\ntitle: x\n---\n\nthe «body»");
        assert!(!out.contains('\n'), "{out:?}");
        assert_eq!(out, "--- title: x --- the <b>body</b>");
        assert_eq!(snippet_markup("  padded  "), "padded");
    }

    #[test]
    fn snippet_markup_stays_valid_when_markers_are_unbalanced() {
        // A note may contain guillemets of its own, so every shape has to parse.
        for s in ["«open", "close»", "«a «b»", "»«", "«<»", ""] {
            let out = snippet_markup(s);
            assert!(
                pango::parse_markup(&out, '\u{0}').is_ok(),
                "{s:?} -> {out:?}"
            );
        }
    }

    #[test]
    fn a_row_is_named_by_its_file_and_its_folder() {
        assert_eq!(basename("notes/deep/thought.md"), "thought.md");
        assert_eq!(dir_label("notes/deep/thought.md", None), "notes/deep/");
        assert_eq!(dir_label("top.md", None), "");
        // A row that is one match says which line it is, wherever the file sits.
        assert_eq!(
            dir_label("notes/deep/thought.md", Some(12)),
            "notes/deep/ — line 12"
        );
        assert_eq!(dir_label("top.md", Some(3)), "line 3");
    }

    #[test]
    fn a_ranked_answer_is_one_row_per_match_and_a_tail_row() {
        let hit = |line: Option<u32>, snippet: &str, at: Range<usize>, more: usize| SearchHit {
            rel_path: "notes/a.md".into(),
            title: None,
            snippet: snippet.into(),
            at: Some(at),
            line,
            more,
        };
        let rows = fts_rows(vec![
            hit(Some(2), "one «ferris» here", 4..10, 0),
            hit(Some(7), "and «ferris» again", 40..46, 3),
            SearchHit {
                rel_path: "b.md".into(),
                title: Some("Ferris".into()),
                snippet: "# «Ferris»".into(),
                at: None,
                line: None,
                more: 0,
            },
        ]);
        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.dir.as_str(), r.at.clone()))
            .collect();
        assert_eq!(
            seen,
            [
                ("a.md", "notes/ — line 2", Some(4..10)),
                ("a.md", "notes/ — line 7", Some(40..46)),
                // The tail row opens the file where its first listed match is.
                ("", "+3 more in this file", Some(4..10)),
                // A hit on a title alone has no line and no place in the body.
                ("b.md", "", None),
            ]
        );
        assert_eq!(rows[0].snippet, "one <b>ferris</b> here");
    }

    #[test]
    fn a_grep_row_marks_the_match_and_then_the_replacement() {
        // A colour name, not a hex literal: DESIGN.md's pre-flight grep allows neither outside
        // `theme.rs`, and pango parses both.
        let plain = match_markup("a <b> c", 2..5, None, "teal");
        assert_eq!(plain, "a <b>&lt;b&gt;</b> c");
        assert!(pango::parse_markup(&plain, '\u{0}').is_ok());

        let replaced = match_markup("a <b> c", 2..5, Some("&x"), "teal");
        assert!(replaced.contains("<s>&lt;b&gt;</s>"), "{replaced}");
        assert!(replaced.contains(">&amp;x</span>"), "{replaced}");
        assert!(pango::parse_markup(&replaced, '\u{0}').is_ok());
    }

    #[test]
    fn the_slower_halves_wait_for_the_typing_to_stop() {
        assert_eq!((DEBOUNCE + WIDEN_AFTER).as_millis(), 400);
    }

    #[test]
    fn walked_rows_come_under_a_heading_that_opens_nothing() {
        let re = search::pattern("zorblat", Options::default()).unwrap();
        assert!(walked_rows(Vec::new(), &re, "teal").is_empty());
        let walked = vec![Match {
            rel_path: "node_modules/dep.js".into(),
            title: None,
            line: 1,
            line_text: "// zorblat".into(),
            range: 3..10,
            offset: 3,
            more: 0,
        }];
        let rows = walked_rows(walked, &re, "teal");
        let seen: Vec<_> = rows
            .iter()
            .map(|r| (r.rel_path.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(seen, [("", NOT_INDEXED), ("node_modules/dep.js", "dep.js")]);
        assert_eq!(rows[1].snippet, "// <b>zorblat</b>");
    }

    #[test]
    fn the_count_says_one_in_the_singular_and_a_cut_number_as_a_floor() {
        assert_eq!(count_label(1, 1, false, false), "1 result in 1 file");
        assert_eq!(count_label(12, 3, false, false), "12 results in 3 files");
        // An exact scan counts every match the index holds past the cap, but not the files.
        assert_eq!(
            count_label(340, 20, false, true),
            "340 results in 20+ files"
        );
        assert_eq!(
            count_label(100, 20, true, true),
            "100+ results in 20+ files"
        );
    }
}
