//! The Search pane: the box, the toggles, the replace row and the query loop behind them.
//!
//! Search runs off the main loop. [`Data::search`] is called on a worker thread, so a full-vault
//! query never costs a keystroke; a bar pulsing above the results says one is running and the
//! previous results stay on screen until the new ones arrive. A query cannot be called back once
//! it is out, so a newer one simply supersedes it: each carries the number it was asked under,
//! and an answer arriving under a newer number is dropped rather than painted.

use super::OnOpen;
use crate::dialogs::alert;
use crate::widgets::{Debounce, Pulse, scroller, status_page};
use accent_core::index::{Match, SearchHit};
use accent_core::path::{basename, parent_dir};
use accent_core::search::{self, Options, Regex};
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, Ref};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Same value as the palette and the switcher (DESIGN.md, Motion): long enough to swallow a burst
/// of keystrokes, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// One step of the search progress bar. GTK4 has no indeterminate mode, so the bar is stepped by
/// a timer of ours; at the default pulse step this crosses the trough in about two seconds.
const PULSE: Duration = Duration::from_millis(80);
/// How many notes Replace All rewrites without asking first. A rewrite cannot be undone and
/// reaches notes nobody has open, which is the choice DESIGN.md's States section keeps an
/// `AdwAlertDialog` for; the one note whose every match the pane is already showing struck
/// through is the case where the preview *is* the confirmation.
const CONFIRM_ABOVE: usize = 1;
/// How many pulses a query has to outlive before its bar is drawn at all (DESIGN.md, Loading).
/// Nothing else in the window starts a search, so a query the user did not ask for — the requery
/// a changed file triggers under a finished search — is over inside this and never draws one.
const SHOW_AFTER: u32 = 2;
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
}

/// What a [`Query`] answered. The `usize` is how many of the matches a Replace All would rewrite,
/// which the capped list cannot give and which is smaller than the list: the rewrite is notes
/// only, while the rows reach every text file.
pub enum Answer {
    Fts(Vec<SearchHit>),
    Grep(Vec<Match>, usize),
}

/// What the Search pane asks of the index.
// Boxed closures returning an answer are the whole point of this struct; a type alias per field
// would only hide the signature the caller has to write.
#[allow(clippy::type_complexity)]
pub struct Data {
    /// Runs on a worker thread, so it may touch nothing the main loop owns.
    pub search: Arc<dyn Fn(Query) -> Answer + Send + Sync>,
    /// Rewrite every match in the vault. `literal` says whether `$1` in the replacement is a
    /// capture group or two characters. It writes one note at a time, so it runs off the main
    /// loop and calls `done` there once it has: the pane stays busy until then.
    pub replace_all: Box<dyn Fn(String, Options, String, bool, Box<dyn FnOnce()>)>,
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

/// The folder a result row sits in, with the trailing slash that says it is one; "" at the vault
/// root. A row names the file — `design.md` and the folder holding it — rather than the note's
/// title: the title hides the extension, and two notes titled the same are then one row twice.
fn dir_label(rel_path: &str) -> String {
    match parent_dir(rel_path) {
        "" => String::new(),
        dir => format!("{dir}/"),
    }
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
    body: gtk::Stack,
    results: gio::ListStore,
    /// Which question the rows on screen are meant to answer. Every query takes the next number
    /// before it starts, so an answer arriving under a newer one is dropped instead of painted:
    /// a slow first query — a remote vault, or an All walk — used to hold every keystroke typed
    /// after it until it landed.
    generation: Cell<u64>,
    /// How many queries are on worker threads. A query cannot be called back, so the count comes
    /// down as each one lands, whether its answer was wanted or not.
    running: Cell<usize>,
    /// A Replace All is rewriting the vault. It writes the very files a query would read, so the
    /// pane asks nothing else until it is over.
    replacing: Cell<bool>,
    /// What the button last promised to rewrite, which is what the confirmation says out loud.
    total: Cell<usize>,
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
        let mine = self.generation.get() + 1;
        self.generation.set(mine);
        // The bar tracks what is running in every branch: a query still on a worker thread keeps
        // it pulsing, and the one that lands after the box was cleared takes it down through here.
        if key.text.trim().is_empty() {
            self.entry.remove_css_class("error");
            self.results.remove_all();
            self.body.set_visible_child_name("prompt");
            self.set_busy(self.busy());
            self.set_total(0);
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
            let answer = gio::spawn_blocking(move || run(query)).await;
            search.running.set(search.running.get() - 1);
            let Ok(answer) = answer else {
                search.set_busy(search.busy());
                return tracing::warn!("the search worker panicked");
            };
            tracing::debug!(
                query = key.text,
                grep = key.grep,
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "sidebar query"
            );
            // The box has moved on since this was asked, so a newer query is already on its way
            // with the answer that belongs on screen. Old results stay up until it lands.
            if search.generation.get() == mine {
                search.show(&key, answer);
            }
            search.set_busy(search.busy());
        });
    }

    fn show(&self, key: &Key, answer: Answer) {
        let rows = match answer {
            Answer::Fts(hits) => {
                self.set_total(0);
                hits.into_iter()
                    .map(|hit| Row {
                        name: basename(&hit.rel_path).to_string(),
                        dir: dir_label(&hit.rel_path),
                        snippet: snippet_markup(&hit.snippet),
                        at: hit.at,
                        rel_path: hit.rel_path,
                    })
                    .collect()
            }
            Answer::Grep(hits, total) => {
                self.set_total(total);
                let Ok(re) = compile_regex(key) else {
                    return;
                };
                let replacement = self.replacement();
                let accent = accent_markup_colour();
                grep_rows(
                    hits,
                    &re,
                    replacement.as_deref(),
                    !key.options.regex,
                    &accent,
                )
            }
        };
        self.body
            .set_visible_child_name(if rows.is_empty() { "empty" } else { "results" });
        let objects: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.results.splice(0, self.results.n_items(), &objects);
    }

    /// How many matches the button would rewrite — not how many the query found. The list is
    /// capped and spans every text file; the number is uncapped and counts notes, because that
    /// is what Replace All opens. A vault of source files would otherwise be promised edits that
    /// never happen.
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
            let dialog = alert(
                &format!("Replace in {total} Notes?"),
                "Every match in these notes is rewritten where it stands. This cannot be undone.",
                &[
                    ("cancel", "Cancel", adw::ResponseAppearance::Default),
                    (
                        "replace",
                        "Replace All",
                        adw::ResponseAppearance::Destructive,
                    ),
                ],
                "cancel",
            );
            let search = self.clone();
            return dialog.choose(Some(&self.apply), gio::Cancellable::NONE, move |response| {
                if response == "replace" {
                    search.run_replace_all();
                }
            });
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
        self.replacing.set(true);
        self.set_busy(true);
        self.apply.set_sensitive(false);
        let search = self.clone();
        (self.data.replace_all)(
            key.text.clone(),
            key.options,
            replacement,
            !key.options.regex,
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
        let dir = dir_label(&m.rel_path);
        rows.push(Row {
            name: basename(&m.rel_path).to_string(),
            // "notes/deep/ — line 12"; a file at the vault root has no folder to name.
            dir: match dir.is_empty() {
                true => format!("line {}", m.line),
                false => format!("{dir} — line {}", m.line),
            },
            snippet: match_markup(&m.line_text, m.range, replaced.as_deref(), accent),
            at: Some(at),
            rel_path: m.rel_path.clone(),
        });
        if m.more > 0 {
            rows.push(Row {
                name: String::new(),
                dir: format!("+{} more in this file", m.more),
                snippet: String::new(),
                at: first.as_ref().map(|(_, at)| at.clone()),
                rel_path: m.rel_path,
            });
        }
    }
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
}

pub(super) fn pane(data: &Rc<Data>, on_open: &OnOpen) -> Pane {
    // ponytail: rows are `glib::BoxedAnyObject`s wrapping a `Row` instead of a GObject with typed
    // properties, the same trade `tree.rs` documents. Define a real item type if the row ever
    // needs bindable state.
    let results = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
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
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
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
        // A tail row has no name, and no icon either: it continues the file above it.
        icon.set_icon_name(Some(crate::doc::icon_for(&hit.rel_path)));
        icon.set_visible(!hit.name.is_empty());
        name.set_text(&hit.name);
        dir.set_text(&hit.dir);
        // A tail row is the dim line alone, so the empty second line is taken away rather than
        // left as a gap under it.
        snippet.set_markup(&hit.snippet);
        snippet.set_visible(!hit.snippet.is_empty());
    });

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
                on_open(&row.rel_path, row.at.clone());
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

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search…")
        .hexpand(true)
        .build();
    // A bar spanning the width right above the results, not a spinner beside the entry: the wait
    // belongs to the list that is about to change, and the entry needs the whole sidebar width.
    // It is faded rather than hidden, so the results never shift when a query starts.
    let progress = gtk::ProgressBar::builder().opacity(0.0).build();

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
    // too, while the skipped trees are a walk that only the exact-match path makes.
    let toggles = [
        ("Aa", "Match Case"),
        ("Word", "Match Whole Word"),
        (".*", "Use Regular Expression"),
        (
            "All",
            "Search Ignored Files, and Skipped Ones in an Exact Search",
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
        body: body.clone(),
        results,
        generation: Cell::new(0),
        running: Cell::new(0),
        replacing: Cell::new(false),
        total: Cell::new(0),
    });

    // A toggle is a click rather than a burst, so only what is typed is debounced.
    let debounce = Rc::new(Debounce::new(DEBOUNCE));
    let debounced: Rc<dyn Fn()> = Rc::new({
        let (search, debounce) = (search.clone(), debounce.clone());
        move || {
            let search = search.clone();
            debounce.call(move || search.start());
        }
    });

    entry.connect_search_changed({
        let (search, debounced, debounce) = (search.clone(), debounced.clone(), debounce.clone());
        move |entry| {
            // Clearing the entry is free, so it cancels the pending query and repaints at once.
            if entry.text().trim().is_empty() {
                debounce.cancel();
                return search.start();
            }
            debounced();
        }
    });
    // ponytail: a changed replacement re-runs the whole query, because the rows carry finished
    // markup rather than the matches they were built from. Off the main thread it costs nothing
    // the user can feel; cache the last answer if a huge vault ever makes it visible.
    replace_entry.connect_changed({
        let debounced = debounced.clone();
        move |_| debounced()
    });
    for button in &toggles {
        button.connect_toggled({
            let search = search.clone();
            move |_| search.start()
        });
    }
    replace_toggle.connect_toggled({
        let (search, replace_row) = (search.clone(), replace_row.clone());
        move |toggle| {
            replace_row.set_reveal_child(toggle.is_active());
            search.start();
        }
    });
    apply.connect_clicked({
        let search = search.clone();
        move |_| search.replace_all()
    });

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
    column.append(&progress);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_shorter_than_the_grace_period_never_draws_a_bar() {
        // A requery nobody asked for costs a few milliseconds on a warm index; the wait has to be
        // long enough to cover one and short enough that a real query still reports itself.
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
        assert_eq!(dir_label("notes/deep/thought.md"), "notes/deep/");
        assert_eq!(dir_label("top.md"), "");
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
}
