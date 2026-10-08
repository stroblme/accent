//! Read-only markdown preview: a WebKitGTK view fed by `markdown::to_html`.
//!
//! The pane renders notes that may have been synced from another machine, so it is built to be a
//! dead end: an ephemeral network session, no page JavaScript, a content filter that refuses every
//! load, and a custom `accent:` scheme that only ever hands out files from inside the vault.
//!
//! WebKit cannot read GTK's CSS variables, so the stylesheet is generated in Rust from the same
//! three sources `highlight.rs` uses — foreground, background, accent — and injected as a user
//! stylesheet. The font comes from `editor::default_font` for the same reason. Editor and preview
//! therefore agree by construction, in every theme, accent and font.

use crate::look::{self, Look, Served};
use crate::theme;
use accent_core::markdown::{self, percent_decode};
use accent_core::path::parent_dir;
use accent_core::search::Options;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use webkit6::prelude::*;

/// Prose column width, in `ch`. DESIGN.md asks for 60 to 72 characters, and `ch` is the advance
/// of "0", which is wider than the average letter: measured on this pane, 56ch is 71 characters of
/// Adwaita Sans and about 62 of Cantarell, so both GNOME document fonts land in the range.
const COLUMN_CH: u32 = 56;

/// What WebKit's find is told under the find bar's toggles: wrapping always, without regard to case
/// unless Match Case is down, and Match Whole Word as matches at a word's start, the nearest WebKit
/// has (`foo` finds `food`). A regular expression it cannot match: the query stays literal.
fn find_options(options: Options) -> webkit6::FindOptions {
    let mut find = webkit6::FindOptions::WRAP_AROUND;
    if !options.case {
        find |= webkit6::FindOptions::CASE_INSENSITIVE;
    }
    if options.word {
        find |= webkit6::FindOptions::AT_WORD_STARTS;
    }
    find
}
/// WebKit's own guard against a query that matches the whole page; the counter says "500+" past it.
const FIND_LIMIT: u32 = 500;

/// How long a WebKit view nobody is using is kept, and its web process of 60 to 120 MB: a preview
/// out of sight, the diagram formulas' typesetter with nothing to typeset. Long enough that Split
/// view toggled off and on again, or the next formula of the diagram being drawn, finds it still
/// there. Two seconds under `ACCENT_BENCH_WEBIDLE`, whose drill waits it out.
pub fn idle_for() -> std::time::Duration {
    #[cfg(feature = "bench")]
    if std::env::var_os("ACCENT_BENCH_WEBIDLE").is_some() {
        return std::time::Duration::from_secs(2);
    }
    std::time::Duration::from_secs(5 * 60)
}

/// Tracing target for what the page's own JavaScript says, so
/// `RUST_LOG=accent::preview=debug accent <vault>` gives the preview's console and nothing else.
/// Its own target for the reason `accent::saves` has one: the answer is a handful of lines.
const PREVIEW: &str = "accent::preview";

/// The name the page posts its console through; spelled again inside [`CONSOLE_SCRIPT`], which is
/// a JavaScript literal and cannot interpolate a Rust constant.
const LOG_HANDLER: &str = "accentLog";

/// WebKit content-blocker rules: refuse every load, then re-allow our own scheme. `decide_policy`
/// only sees navigations, so without this a note could still reach the network through a
/// subresource — an `<img>` tracking pixel in raw HTML being the obvious one.
const BLOCK_NETWORK: &str = concat!(
    r#"[{"trigger":{"url-filter":".*"},"action":{"type":"block"}},"#,
    r#"{"trigger":{"url-filter":"^accent:"},"action":{"type":"ignore-previous-rules"}}]"#
);

/// Scroll sync. The markers `to_html` emits sit *inside* their block (`<p><span data-line="5">…`),
/// so the marker itself is hidden and the block around it is what gets scrolled into view.
const SCROLL_SCRIPT: &str = r#"
window.__accentScrollToLine = function (line) {
  var marks = document.querySelectorAll('[data-line]');
  var found = null;
  for (var i = 0; i < marks.length; i++) {
    if (parseInt(marks[i].getAttribute('data-line'), 10) > line) { break; }
    found = marks[i];
  }
  // At or above the first block *is* the top of the document, and scrolling that block into view
  // is not the same thing: it takes the page's own top padding and the block's margin off screen,
  // so a note whose cursor is on line 1 would open already scrolled past its own beginning.
  if (!found || found === marks[0]) { return window.scrollTo(0, 0); }
  (found.parentElement || found).scrollIntoView({ block: 'start' });
};
"#;

/// Relay the page's own errors back into this process.
///
/// WebKit's `enable-write-console-messages-to-stdout` writes on the *web process's* stdout, which
/// a normal session never sees, so a script message handler is the only route home. `console.error`
/// is wrapped rather than replaced, so anything watching from a web inspector still sees it.
const CONSOLE_SCRIPT: &str = r#"
(function () {
  var post = function (kind, text) {
    try { window.webkit.messageHandlers.accentLog.postMessage(kind + '\t' + text); } catch (e) {}
  };
  window.onerror = function (msg, src, line, col) { post('error', msg + ' (' + line + ':' + col + ')'); };
  window.addEventListener('unhandledrejection', function (e) { post('error', 'unhandled rejection: ' + e.reason); });
  ['error', 'warn'].forEach(function (level) {
    var inner = console[level];
    console[level] = function () {
      post(level, Array.prototype.join.call(arguments, ' '));
      inner.apply(console, arguments);
    };
  });
})();
"#;

/// Mermaid plus the bootstrap that draws the diagrams, injected only into a note that has one.
///
/// The library is vendored rather than fetched: the preview's content filter blocks every load
/// that is not `accent:`, and a diagram must render with no network at all. Mermaid asks for
/// nothing at runtime, so nothing else in the hardening moves.
///
/// The bootstrap is Android's too (`vendor/mermaid/bootstrap.js`); what is the desktop's alone is
/// the theme by the page's lightness and the scroll-sync marker each fence carries.
const MERMAID: &str = concat!(
    include_str!("../../../vendor/mermaid/mermaid.min.js"),
    "\n",
    include_str!("../../../vendor/mermaid/bootstrap.js"),
    r#"
(function () {
  var rgb = getComputedStyle(document.documentElement).backgroundColor.match(/\d+/g) || [255, 255, 255];
  var luma = (0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]) / 255;
  // Kept where a print or an export can wait for the diagrams before taking the page.
  window.__accentDrawn = accentDiagrams(luma < 0.5 ? 'dark' : 'neutral', function (fence) {
    // The block's scroll-sync marker sits inside the fence and has to outlive it.
    var mark = fence.querySelector('[data-line]');
    if (mark) { fence.parentElement.insertBefore(mark, fence); }
  });
})();
"#
);

struct Inner {
    view: webkit6::WebView,
    content: webkit6::UserContentManager,
    /// Kept for its cache, which holds every image served to the page until it is cleared.
    session: webkit6::NetworkSession,
    assets: Rc<Assets>,
    /// The look the page's images were last served under, so a restyle can say whether they
    /// have to be served again.
    look: Cell<Look>,
    /// The sheet currently injected, so `restyle` can replace instead of stack.
    sheet: RefCell<Option<webkit6::UserStyleSheet>>,
    /// [`MERMAID`] while a note with a diagram is shown, `None` otherwise.
    mermaid: RefCell<Option<webkit6::UserScript>>,
    /// Whether a page may load yet; see [`Gate`].
    gate: RefCell<Gate>,
    /// How many renders were asked for: a page back from its worker loads only if it is the
    /// latest, so a long note's render never lands over a shorter one asked for after it.
    renders: Cell<u64>,
    loaded: Cell<bool>,
    /// A line asked for while the page was still loading.
    pending: Cell<Option<u32>>,
    /// What the find bar is looking for, and under which of its toggles, kept because every
    /// re-render reloads the page and WebKit's find dies with it.
    query: RefCell<Option<String>>,
    options: Cell<Options>,
    /// Matches WebKit last counted, and which of them the reader is on (1-based, 0 for none).
    /// `WebKitFindController` reports a total and never a position, so the position is ours to
    /// keep: [`Inner::refind`] starts every fresh search from the top, and the two step methods
    /// walk it the same way WebKit walks the page.
    total: Cell<u32>,
    at: Cell<u32>,
    /// Where the readout goes, called with the label rather than the numbers so the bar does not
    /// have to know how a preview counts.
    report: RefCell<Option<Report>>,
    /// Whether the count handler is already on WebKit's find controller. The reporter itself is
    /// replaceable, the handler is not: connecting a second one would count every match twice.
    counting: Cell<bool>,
    /// Whether WebKit's process died under the page since a page last finished loading; see
    /// [`Preview::connect_lost`].
    lost: Cell<bool>,
}

impl Inner {
    /// Put `body` on the page as `rel`'s.
    fn load(&self, rel: &str, body: &str) {
        *self.assets.note.borrow_mut() = rel.to_string();
        self.set_mermaid(body.contains("language-mermaid"));
        self.loaded.set(false);
        self.view.load_html(&document(body), Some(&base_uri(rel)));
    }

    /// The network filter is in, or could not be: load what waited for it.
    fn unlock(&self, filter: &Filter) {
        let filter = filter
            .as_ref()
            .map(|f| self.content.add_filter(f))
            .map_err(Clone::clone);
        let next = self.gate.borrow_mut().settle(filter);
        if let Some((rel, body)) = next {
            self.load(&rel, &body);
        }
    }

    fn scroll(&self, line: u32) {
        self.view.evaluate_javascript(
            &format!("window.__accentScrollToLine({line})"),
            None,
            None,
            gio::Cancellable::NONE,
            report_js,
        );
    }

    /// Run the stored find query against the page that is up now. Searching a page that has not
    /// finished loading silently finds nothing — measured under Xvfb: the same query returns 0
    /// matches issued a second after `load_html` and 3 issued five seconds later — so every route
    /// to a search goes through here and the load handler calls it again.
    fn refind(&self) {
        let query = self.query.borrow();
        let (Some(finder), Some(text)) = (
            self.view.find_controller(),
            query.as_deref().filter(|t| !t.is_empty()),
        ) else {
            return;
        };
        // WebKit searches forward from whatever is selected, so a query changed after the reader
        // has stepped a few matches would land somewhere in the middle and there would be no
        // saying where. Dropping the selection first makes every fresh search land on match one,
        // which is what lets the counter say "1 of 12" and mean it. Fire and forget: the script
        // and the find both go to the web process over the same connection, in this order.
        self.view.evaluate_javascript(
            "window.getSelection().removeAllRanges()",
            None,
            None,
            gio::Cancellable::NONE,
            report_js,
        );
        // Counting first is the order WebKit's own MiniBrowser uses; `search` reports no total.
        let options = find_options(self.options.get()).bits();
        finder.count_matches(text, options, FIND_LIMIT);
        finder.search(text, options, FIND_LIMIT);
    }

    /// Move the counter one match on and say so.
    fn step(&self, forward: bool) {
        self.at
            .set(stepped(self.at.get(), self.total.get(), forward));
        self.say();
    }

    fn say(&self) {
        let label = match self.query.borrow().as_deref() {
            Some(text) if !text.is_empty() => matches_label(self.at.get(), self.total.get()),
            // Nothing asked is not "No results"; the editor's readout is blank there too.
            _ => String::new(),
        };
        if let Some(report) = self.report.borrow().as_ref() {
            report(&label);
        }
    }

    /// Add or drop the mermaid script. It is 3.4 MB of JavaScript to parse, so a note without a
    /// diagram must not carry it; `remove_script` takes the one script, leaving `SCROLL_SCRIPT`.
    fn set_mermaid(&self, wanted: bool) {
        let mut slot = self.mermaid.borrow_mut();
        if wanted == slot.is_some() {
            return;
        }
        match slot.take() {
            Some(script) => self.content.remove_script(&script),
            None => {
                let script = webkit6::UserScript::new(
                    MERMAID,
                    webkit6::UserContentInjectedFrames::TopFrame,
                    webkit6::UserScriptInjectionTime::End,
                    &[],
                    &[],
                );
                self.content.add_script(&script);
                *slot = Some(script);
            }
        }
    }
}

/// What one of our own `evaluate_javascript` calls reported. An error thrown inside an injected
/// user script reaches `window.onerror` as a bare "Script error." — WebKit scrubs it, the script
/// not being the document's own — so the calls this file makes are logged where the detail is.
fn report_js(result: Result<webkit6::javascriptcore::Value, glib::Error>) {
    if let Err(e) = result {
        tracing::warn!(target: PREVIEW, "{e}");
    }
}

/// One match on from `at` (1-based), wrapping at either end because [`find_options`] tells WebKit
/// to wrap. 0 in, 0 out: nothing found is nowhere to step.
fn stepped(at: u32, total: u32, forward: bool) -> u32 {
    match (total, forward) {
        (0, _) => 0,
        (total, true) => at % total + 1,
        (total, false) if at <= 1 => total,
        (_, false) => at - 1,
    }
}

/// "3 of 12", the way the editor and the PDF reader both say it.
///
/// `at` is 1-based, 0 meaning nothing is selected. A total that reaches [`FIND_LIMIT`] is a floor
/// rather than a count — WebKit stops looking there — so the position goes with it: "7 of 500" on
/// a page holding nine hundred matches would be wrong in both halves.
fn matches_label(at: u32, total: u32) -> String {
    match (at, total) {
        (_, 0) => "No results".to_string(),
        (_, n) if n >= FIND_LIMIT => format!("{n}+ matches"),
        (0, n) => format!("{n} matches"),
        (at, n) => format!("{at} of {n}"),
    }
}

/// What the preview is allowed to read: a vault-relative path in, the vault file it names (its
/// key) and that file on *this* machine out, `None` when there is none. Every window passes
/// `Vault::fetch`, which is the file itself for a local vault and a copy fetched over ssh for a
/// remote one — so a call can block on the network.
pub type Resolve = dyn Fn(&str) -> Option<(String, PathBuf)> + Send + Sync;

/// Keys the window keeps and its preview reads as it serves: the images inverted, the diagrams
/// whose pictures on the web are drawn.
type Keys = Rc<RefCell<HashSet<String>>>;

/// What the `accent:` scheme serves from.
struct Assets {
    resolve: Arc<Resolve>,
    /// The images the reader inverted, by key: the window's own set, which its image tabs read too.
    inverted: Rc<RefCell<HashSet<String>>>,
    /// How many requests the page has made, for the drills: whether WebKit asks again for an
    /// image it was served before.
    #[cfg(feature = "bench")]
    requests: Cell<u32>,
    /// The keys served since WebKit's memory cache was last cleared, whose bytes it may answer a
    /// render with even after the file has changed.
    served: RefCell<HashSet<String>>,
    /// A page for print or export ([`Preview::for_paper`]): white paper, images as their files.
    paper: bool,
    /// The key of the note on the page: a loose one's images are read beside it
    /// ([`resolve_asset`]).
    note: RefCell<String>,
    /// The diagrams whose pictures on the web an embed draws, by key: the window's own set.
    web: Keys,
}

/// Where the find readout goes; see [`Preview::connect_found`].
type Report = Box<dyn Fn(&str)>;

pub struct Preview {
    inner: Rc<Inner>,
    widget: gtk::Widget,
}

impl Preview {
    /// `resolve` turns a vault-relative asset path into a file on this machine; [`resolve_asset`]
    /// says which half of the containment guarantee is whose. `inverted` holds the images the
    /// reader inverted, which are served the other way round from what the theme asks; `web` the
    /// diagrams whose pictures on the web an embed of them draws. `on_open`
    /// fires when the reader clicks a link into the vault, with a wikilink's target as written or
    /// a markdown link's vault path, and the `#anchor` if there is one; `on_invert` when the
    /// reader asks the image menu to invert an image, with its key.
    ///
    /// ponytail: every `Preview` builds its own `WebContext`, so one per tab means one WebKit
    /// process group per tab. Sharing a context (and its registered scheme) across previews is the
    /// upgrade path if tab memory ever shows up in a measurement.
    pub fn new(
        resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        inverted: Keys,
        web: Keys,
        on_open: impl Fn(&str) + 'static,
        on_invert: impl Fn(&str) + 'static,
    ) -> Preview {
        Self::build(Arc::new(resolve), inverted, web, on_open, on_invert, false)
    }

    /// A page to print or export a note from, never shown: the note on the light theme's white
    /// paper whatever the window's theme, its images as their files are, its diagrams in mermaid's
    /// `neutral` theme, and nothing in it followed anywhere.
    pub fn for_paper(resolve: Arc<Resolve>, web: Keys) -> Preview {
        Self::build(resolve, Rc::default(), web, |_| {}, |_| {}, true)
    }

    fn build(
        resolve: Arc<Resolve>,
        inverted: Keys,
        web: Keys,
        on_open: impl Fn(&str) + 'static,
        on_invert: impl Fn(&str) + 'static,
        paper: bool,
    ) -> Preview {
        // `register_uri_scheme` asks only for `'static` and calls back on the main loop, so an `Rc`
        // would be enough to hold the resolver there — but every request hands it to a
        // `gio::spawn_blocking` worker, and crossing a thread needs `Send`. Hence `Arc`, and the
        // `Send + Sync` bound that an `Arc` of a shared closure requires.
        let assets = Rc::new(Assets {
            resolve,
            inverted,
            #[cfg(feature = "bench")]
            requests: Cell::new(0),
            served: RefCell::default(),
            paper,
            note: RefCell::default(),
            web,
        });

        let context = webkit6::WebContext::new();
        context.register_uri_scheme(
            "accent",
            glib::clone!(
                #[strong]
                assets,
                move |request| serve(&assets, request)
            ),
        );

        // Ephemeral: no cookie jar, no disk cache, nothing that outlives the window.
        let session = webkit6::NetworkSession::new_ephemeral();

        let settings = webkit6::Settings::new();
        // `<script>` in a note never runs, while our own injected script still does.
        settings.set_enable_javascript_markup(false);
        settings.set_enable_media(false);
        settings.set_enable_webgl(false);
        settings.set_enable_webaudio(false);
        settings.set_enable_html5_local_storage(false);
        settings.set_enable_html5_database(false);
        settings.set_enable_page_cache(false);
        settings.set_enable_back_forward_navigation_gestures(false);

        let content = webkit6::UserContentManager::new();
        content.add_script(&webkit6::UserScript::new(
            SCROLL_SCRIPT,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
        install_console(&content);

        let view = webkit6::WebView::builder()
            .web_context(&context)
            .network_session(&session)
            .user_content_manager(&content)
            .settings(&settings)
            .hexpand(true)
            .vexpand(true)
            .build();

        let inner = Rc::new(Inner {
            view,
            content,
            session,
            assets,
            look: Cell::new(Look::now()),
            sheet: RefCell::new(None),
            mermaid: RefCell::new(None),
            gate: RefCell::new(Gate::Waiting(None)),
            renders: Cell::new(0),
            loaded: Cell::new(false),
            pending: Cell::new(None),
            query: RefCell::new(None),
            options: Cell::new(Options::default()),
            total: Cell::new(0),
            at: Cell::new(0),
            report: RefCell::new(None),
            counting: Cell::new(false),
            lost: Cell::new(false),
        });
        network_filter(glib::clone!(
            #[weak]
            inner,
            move |filter| inner.unlock(filter)
        ));

        inner.view.connect_load_changed(glib::clone!(
            #[weak]
            inner,
            move |_, event| {
                if event == webkit6::LoadEvent::Finished {
                    inner.loaded.set(true);
                    inner.lost.set(false);
                    if let Some(line) = inner.pending.take() {
                        inner.scroll(line);
                    }
                    inner.refind();
                }
            }
        ));

        inner
            .view
            .connect_decide_policy(move |view, decision, kind| {
                decide(view, decision, kind, &on_open)
            });
        image_menu(&inner, on_invert);

        // `color()` only resolves the theme foreground once the widget is mapped, so restyle
        // then as well — same reasoning as `editor.rs`.
        inner.view.connect_map(glib::clone!(
            #[weak]
            inner,
            move |_| Preview::apply_style(&inner)
        ));

        let preview = Preview {
            widget: inner.view.clone().upcast(),
            inner,
        };
        preview.restyle();
        preview
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    /// Render `text`, resolving relative links as if the note lived at `rel`.
    ///
    /// Re-rendering starts the page from the top; the caller restores the reading position with
    /// [`Preview::scroll_to_line`]. `to_html` runs on a worker, a note of a megabyte taking some
    /// 20 ms, and only the latest render asked for loads. One asked for before the network filter
    /// is on the view waits for it ([`Gate`]).
    pub fn render(&self, rel: &str, text: &str) {
        let render = self.inner.renders.get() + 1;
        self.inner.renders.set(render);
        // The page up is not this render's: a scroll asked for meanwhile waits for it.
        self.inner.loaded.set(false);
        let (rel, text) = (rel.to_string(), text.to_string());
        let inner = Rc::downgrade(&self.inner);
        glib::spawn_future_local(async move {
            let body = crate::work::off_thread("preview", move || markdown::to_html(&text)).await;
            let Some((inner, body)) = inner
                .upgrade()
                .zip(body)
                .filter(|(inner, _)| inner.renders.get() == render)
            else {
                return;
            };
            let next = inner.gate.borrow_mut().pass((rel, body));
            if let Some((rel, body)) = next {
                inner.load(&rel, &body);
            }
        });
    }

    /// Why the page shows no note: the network filter could not be installed.
    pub fn refused(&self) -> Option<String> {
        match &*self.inner.gate.borrow() {
            Gate::Shut(why) => Some(why.clone()),
            _ => None,
        }
    }

    /// Scroll so the block containing source line `line` is visible.
    pub fn scroll_to_line(&self, line: u32) {
        if self.inner.loaded.get() {
            self.inner.scroll(line);
        } else {
            self.inner.pending.set(Some(line));
        }
    }

    /// Rebuild the stylesheet from the current GNOME theme, accent colour and document font, and
    /// say whether the page's images want serving again ([`Preview::forget_images`]).
    pub fn restyle(&self) -> bool {
        Self::apply_style(&self.inner);
        let look = Look::now();
        self.inner.look.replace(look) != look
    }

    /// Drop every image WebKit keeps from the page, then `then`, which renders the note again:
    /// a new look or an inverted image is served only to a page that asks for it once more.
    pub fn forget_images(&self, then: impl FnOnce() + 'static) {
        self.inner.assets.served.borrow_mut().clear();
        let Some(data) = self.inner.session.website_data_manager() else {
            return then();
        };
        // The callback wants `Send`, and comes back on this thread; the guard carries `then`,
        // which is not, across a boundary it never really crosses.
        let then = glib::thread_guard::ThreadGuard::new(then);
        let span = glib::TimeSpan::from_seconds(0);
        data.clear(
            webkit6::WebsiteDataTypes::MEMORY_CACHE,
            span,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(e) = result {
                    tracing::warn!(target: PREVIEW, "cannot clear the image cache: {e}");
                }
                (then.into_inner())();
            },
        );
    }

    /// Whether WebKit may still hold `key`'s bytes as they were when it was served, or those of a
    /// file under the folder `key`, so a change, a removal or a rename of it wants
    /// [`Preview::forget_images`].
    pub fn holds(&self, key: &str) -> bool {
        let folder = format!("{key}/");
        let served = self.inner.assets.served.borrow();
        served.contains(key) || served.iter().any(|k| k.starts_with(&folder))
    }

    /// How many requests the page has made so far.
    #[cfg(feature = "bench")]
    pub fn requests(&self) -> u32 {
        self.inner.assets.requests.get()
    }

    /// The web view, for a drill that reads the page itself.
    pub fn view(&self) -> &webkit6::WebView {
        &self.inner.view
    }

    /// Call `f`, which renders the note again, when WebKit's process dies under the page — a
    /// crash, or past its memory limit — which leaves the view on its last frame, scrolling and
    /// following nothing, until something renders it again. Once until a page has finished
    /// loading since: a note that brings the process down as it loads waits for its next render
    /// rather than bringing it down again and again.
    pub fn connect_lost(&self, f: impl Fn() + 'static) {
        let inner = Rc::downgrade(&self.inner);
        self.inner
            .view
            .connect_web_process_terminated(move |_, reason| {
                tracing::warn!(target: PREVIEW, "the preview's web process ended: {reason:?}");
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                if reason != webkit6::WebProcessTerminationReason::TerminatedByApi
                    && !inner.lost.replace(true)
                {
                    f();
                }
            });
    }

    fn apply_style(inner: &Inner) {
        // WebKit cannot resolve `var(--view-bg-color)`, so `theme` hands out the literal the
        // rest of the window resolves to under the current theme (DESIGN.md, Colour).
        let (bg, fg) = match inner.assets.paper {
            true => theme::paper(),
            false => (
                theme::view_bg(adw::StyleManager::default().is_dark()),
                inner.view.color(),
            ),
        };
        let css = stylesheet(fg, bg);

        if let Some(old) = inner.sheet.borrow_mut().take() {
            inner.content.remove_style_sheet(&old);
        }
        let sheet = webkit6::UserStyleSheet::new(
            &css,
            webkit6::UserContentInjectedFrames::TopFrame,
            webkit6::UserStyleLevel::User,
            &[],
            &[],
        );
        inner.content.add_style_sheet(&sheet);
        *inner.sheet.borrow_mut() = Some(sheet);
        // Paint the view itself too, or dark mode flashes white between load and first paint.
        if let Ok(rgba) = bg.parse::<gdk::RGBA>() {
            inner.view.set_background_color(&rgba);
        }
    }

    /// The document zoom, shared with the editor. WebKit keeps it across loads and `apply_style`
    /// never touches it, so a stylesheet rebuild cannot undo it.
    pub fn set_zoom(&self, zoom: f64) {
        self.inner.view.set_zoom_level(zoom);
    }

    // --- find ----------------------------------------------------------------------------
    //
    // Presentation mode hides the editor under the rendered page, so Ctrl+F has to address it
    // instead of the buffer. WebKit does the searching; the find bar only decides which of the
    // two it is talking to.

    /// Highlight and jump to the first match of `text` under the find bar's toggles; an empty query
    /// clears the search. Both are remembered, so they survive the re-render an edit triggers.
    pub fn find(&self, text: &str, options: Options) {
        *self.inner.query.borrow_mut() = Some(text.to_string());
        self.inner.options.set(options);
        if text.is_empty() {
            return self.find_clear();
        }
        if self.inner.loaded.get() {
            self.inner.refind();
        }
    }

    pub fn find_next(&self) {
        if let Some(finder) = self.inner.view.find_controller() {
            finder.search_next();
            self.inner.step(true);
        }
    }

    pub fn find_previous(&self) {
        if let Some(finder) = self.inner.view.find_controller() {
            finder.search_previous();
            self.inner.step(false);
        }
    }

    pub fn find_clear(&self) {
        *self.inner.query.borrow_mut() = None;
        self.inner.total.set(0);
        self.inner.at.set(0);
        self.inner.say();
        if let Some(finder) = self.inner.view.find_controller() {
            finder.search_finish();
        }
    }

    /// Called with the readout — "3 of 12", "No results" — after a search and after every step.
    pub fn connect_found(&self, f: impl Fn(&str) + 'static) {
        *self.inner.report.borrow_mut() = Some(Box::new(f));
        let Some(finder) = self.inner.view.find_controller() else {
            return;
        };
        // The handler reads the reporter out of `inner` when it fires, so one is enough however
        // many times this is called; a second would report each count twice.
        if self.inner.counting.replace(true) {
            return;
        }
        finder.connect_counted_matches(glib::clone!(
            #[weak(rename_to = inner)]
            self.inner,
            move |_, count| {
                inner.total.set(count);
                // Every count is preceded by a search that starts from the top, so the reader is
                // on the first match whenever there is one.
                inner.at.set((count > 0) as u32);
                inner.say();
            }
        ));
    }
}

// ------------------------------------------------------------------------------------- console

/// Hand the page a way to report its own JavaScript errors, and log what comes back.
///
/// An error in [`SCROLL_SCRIPT`] or in the mermaid bootstrap is otherwise entirely silent:
/// `set_enable_write_console_messages_to_stdout` writes on the web process's stdout and produced
/// nothing here. An uncaught error is a `warn!` because it means a feature of the pane is not
/// working; `console.warn`/`console.error` are `debug!` because a vendored library is entitled to
/// grumble — mermaid says nothing at all about a fence it cannot parse, which is how this was
/// checked.
fn install_console(content: &webkit6::UserContentManager) {
    if !content.register_script_message_handler(LOG_HANDLER, None) {
        return tracing::warn!(target: PREVIEW, "no console relay: {LOG_HANDLER} is taken");
    }
    content.connect_script_message_received(Some(LOG_HANDLER), |_, value| {
        let message = value.to_str();
        match message.split_once('\t') {
            Some(("error", text)) => tracing::warn!(target: PREVIEW, "{text}"),
            Some((level, text)) => tracing::debug!(target: PREVIEW, "{level}: {text}"),
            None => tracing::debug!(target: PREVIEW, "{message}"),
        }
    });
    content.add_script(&webkit6::UserScript::new(
        CONSOLE_SCRIPT,
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
}

// ------------------------------------------------------------------------------- network policy

/// [`BLOCK_NETWORK`] as WebKit compiled it, or why it could not.
pub(crate) type Filter = Result<webkit6::UserContentFilter, String>;

/// The app's one compiled [`Filter`]: being compiled for those waiting, or done.
enum Compiled {
    Waiting(Vec<Waiter>),
    Done(Filter),
}

/// Who wants the filter; see [`network_filter`].
type Waiter = Box<dyn FnOnce(&Filter)>;

thread_local! {
    static FILTER: RefCell<Option<Compiled>> = const { RefCell::new(None) };
}

/// Call `then` with the network filter, which every view loading what a file holds puts on
/// itself before its first load. WebKit compiles it asynchronously and disk-backed, the only
/// API it offers, so the first view to ask waits a moment and every one after has it at once.
pub(crate) fn network_filter(then: impl FnOnce(&Filter) + 'static) {
    let ready = FILTER.with_borrow_mut(|compiled| match compiled.get_or_insert_with(compile) {
        Compiled::Waiting(waiting) => {
            waiting.push(Box::new(then));
            None
        }
        Compiled::Done(filter) => Some((then, filter.clone())),
    });
    if let Some((then, filter)) = ready {
        then(&filter);
    }
}

/// Start compiling [`BLOCK_NETWORK`] into the app's cache; its callback, never called before this
/// returns, hands the filter to everyone waiting for it.
fn compile() -> Compiled {
    let dir = glib::user_cache_dir()
        .join("accent")
        .join("content-filters");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Compiled::Done(Err(not_installed(format!(
            "no content-filter store at {}: {e}",
            dir.display()
        ))));
    }
    webkit6::UserContentFilterStore::new(&dir.to_string_lossy()).save(
        "accent-block-network",
        &glib::Bytes::from_static(BLOCK_NETWORK.as_bytes()),
        gio::Cancellable::NONE,
        |result| {
            let filter = result.map_err(|e| not_installed(e.to_string()));
            let done = Compiled::Done(filter.clone());
            if let Some(Compiled::Waiting(waiting)) = FILTER.with_borrow_mut(|c| c.replace(done)) {
                for then in waiting {
                    then(&filter);
                }
            }
        },
    );
    Compiled::Waiting(Vec::new())
}

/// Say loudly that the filter is missing: no page loads without it, and one that did could talk
/// to the network.
fn not_installed(why: String) -> String {
    tracing::error!("preview: network filter not installed: {why}");
    why
}

/// What stands between a render and the page: the network filter, which arrives a moment after
/// the view does. No page loads before it is on the view, so not even the first note's images
/// reach the network, and no note at all if it could not be put there.
enum Gate {
    /// On its way; the latest page asked for meanwhile.
    Waiting(Option<Page>),
    Open,
    /// It could not be installed, and why.
    Shut(String),
}

/// A page to load: the note's vault path and its HTML body.
type Page = (String, String);

impl Gate {
    /// What to load for `page` now: itself once the filter is in, a page saying why there is
    /// none once it could not be, and nothing while it is on its way, `page` waiting in place of
    /// whatever waited before.
    fn pass(&mut self, page: Page) -> Option<Page> {
        match self {
            Gate::Waiting(waiting) => {
                *waiting = Some(page);
                None
            }
            Gate::Open => Some(page),
            Gate::Shut(why) => Some((page.0, refusal(why))),
        }
    }

    /// The filter is in, or could not be: what to load for the page that waited, if one did.
    fn settle(&mut self, filter: Result<(), String>) -> Option<Page> {
        let next = match filter {
            Ok(()) => Gate::Open,
            Err(why) => Gate::Shut(why),
        };
        match std::mem::replace(self, next) {
            Gate::Waiting(Some(page)) => self.pass(page),
            _ => None,
        }
    }
}

/// The page in place of a note while the network filter is missing: why, and nothing to fetch.
fn refusal(why: &str) -> String {
    format!(
        "<p>No preview: the filter that keeps notes off the network could not be installed \
         ({}).</p>",
        glib::markup_escape_text(why)
    )
}

/// Route a navigation: links into the vault back to the app, a web, mail, phone or message link
/// to the system, our own document load and an anchor into it through, everything else nowhere.
fn decide(
    view: &webkit6::WebView,
    decision: &webkit6::PolicyDecision,
    kind: webkit6::PolicyDecisionType,
    on_open: &impl Fn(&str),
) -> bool {
    use webkit6::PolicyDecisionType as Type;
    if !matches!(kind, Type::NavigationAction | Type::NewWindowAction) {
        return false;
    }
    let Some(action) = decision
        .downcast_ref::<webkit6::NavigationPolicyDecision>()
        .and_then(|d| d.navigation_action())
    else {
        return false;
    };
    let uri = action
        .request()
        .and_then(|r| r.uri())
        .unwrap_or_default()
        .to_string();

    match accent_uri(&uri) {
        // Our own `load_html`, which arrives as the base URI and never as a click.
        Some(("file", _)) if action.navigation_type() != webkit6::NavigationType::LinkClicked => {
            return false;
        }
        // An in-note `[text](#slug)`: WebKit scrolls to the heading `to_html` gave that `id`.
        _ if view.uri().is_some_and(|page| same_page(&uri, &page)) => return false,
        // A wikilink, or a markdown link the base URI has already made a vault path. The anchor
        // rides along for a PDF, which needs the page as well as the file, and for a note, whose
        // heading it names. Rejoined here rather than kept by `accent_uri`, whose asset requests
        // must go on dropping it: an asset is fetched by path and a `#` in one is not a place.
        Some(("open" | "file", target)) => match uri.split_once('#') {
            Some((_, anchor)) if !anchor.is_empty() => {
                on_open(&format!("{target}#{}", percent_decode(anchor)))
            }
            _ => on_open(&target),
        },
        _ if launches(&uri, action.is_user_gesture()) => {
            let parent = view.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(parent.as_ref(), gio::Cancellable::NONE, |result| {
                if let Err(e) = result {
                    tracing::warn!("preview: cannot open link: {e}");
                }
            });
        }
        _ => tracing::debug!("preview: refused navigation to {uri}"),
    }
    decision.ignore();
    true
}

/// Whether a navigation leaves the app for the system. Only a click does — a note carrying
/// `<meta http-equiv="refresh">` or a script redirect must not open anything on its own — and only
/// to what the editor's Go to Definition opens too ([`markdown::is_url`]): never `javascript:`,
/// `data:` or `file:`.
fn launches(uri: &str, clicked: bool) -> bool {
    clicked && markdown::is_url(uri)
}

/// Whether `uri` is a place on the page `current` shows: the same document with a `#fragment`.
/// `current` may carry a fragment of its own, from the last anchor followed.
fn same_page(uri: &str, current: &str) -> bool {
    let page = current.split_once('#').map_or(current, |(doc, _)| doc);
    uri.split_once('#').is_some_and(|(doc, _)| doc == page)
}

// ----------------------------------------------------------------------------------- image menu

/// Offer Invert Image Colours in the page's menu over an image of the vault's, handing
/// `on_invert` the image's key once the vault has named the file.
fn image_menu(inner: &Inner, on_invert: impl Fn(&str) + 'static) {
    let action = gio::SimpleAction::new("invert-image", Some(glib::VariantTy::STRING));
    let (assets, on_invert) = (inner.assets.clone(), Rc::new(on_invert));
    action.connect_activate(move |_, rel| {
        let Some(rel) = rel.and_then(|v| v.str()).map(str::to_string) else {
            return;
        };
        let (resolve, note) = (assets.resolve.clone(), assets.note.borrow().clone());
        let on_invert = on_invert.clone();
        glib::spawn_future_local(async move {
            let found =
                crate::work::off_thread("asset", move || resolve_asset(&*resolve, &note, &rel));
            if let Some(Some((key, _))) = found.await {
                on_invert(&key);
            }
        });
    });
    inner.view.connect_context_menu(move |_, menu, hit| {
        let uri = hit.context_is_image().then(|| hit.image_uri()).flatten();
        if let Some(("file", rel)) = uri.as_deref().and_then(accent_uri) {
            menu.append(&webkit6::ContextMenuItem::new_separator());
            menu.append(&webkit6::ContextMenuItem::from_gaction(
                &action,
                "Invert Image Colours",
                Some(&rel.to_variant()),
            ));
        }
        false
    });
}

// ----------------------------------------------------------------------------------- uri scheme

/// Answer one `accent://file/<rel>` request, or fail it. Everything the preview is allowed to see
/// passes through here, so this is the only place a path from a note becomes a path on disk.
///
/// The answer arrives a main-loop turn later. Resolving a remote vault's asset downloads it, which
/// is an ssh round trip, and the handler runs on the main loop — a large image would freeze the
/// window. WebKit documents the way out on `register_uri_scheme`: keep a reference to the request
/// and finish it once the data is there. So the resolving goes to a `gio::spawn_blocking` worker,
/// the same pairing the git pane uses, and only the finishing comes back to the main loop. The
/// worker also decides how an image is shown in the look in force, which may mean recolouring it
/// ([`look::serve`]). A diagram is drawn back on the main loop, where GTK and its typesetter
/// live, as an SVG that then goes through the look as an SVG image does ([`look::serve_svg`]).
fn serve(assets: &Rc<Assets>, request: &webkit6::URISchemeRequest) {
    #[cfg(feature = "bench")]
    assets.requests.set(assets.requests.get() + 1);
    let uri = request.uri().unwrap_or_default();
    let Some(("file", rel)) = accent_uri(&uri) else {
        return deny(request, "not a vault file");
    };
    let page = diagram_page(&uri);
    // The look and the inverted set live on this thread; the worker gets a copy of each.
    let (resolve, request) = (assets.resolve.clone(), request.clone());
    let look = match assets.paper {
        true => Look::paper(),
        false => Look::now(),
    };
    let inverted = assets.inverted.borrow().clone();
    let note = assets.note.borrow().clone();
    let assets = assets.clone();
    glib::spawn_future_local(async move {
        let answer = crate::work::off_thread("asset", move || {
            let (key, path) = resolve_asset(&*resolve, &note, &rel)?;
            let inverted = inverted.contains(&key);
            let served =
                (!accent_core::path::is_diagram(&key)).then(|| look::serve(&path, look, inverted));
            Some((key, path, served, inverted))
        });
        let answer = answer.await;
        if let Some(Some((key, ..))) = &answer {
            assets.served.borrow_mut().insert(key.clone());
        }
        let (path, served) = match answer {
            Some(Some((_, path, Some(served), _))) => (path, served),
            Some(Some((key, path, None, inverted))) => {
                let web = assets.web.borrow().contains(&key);
                match crate::diagram::embed::svg(&path, page.as_deref(), web).await {
                    Some(svg) => (path, look::serve_svg(&svg, look, inverted)),
                    None => return deny(&request, "no such diagram page"),
                }
            }
            Some(None) => return deny(&request, "outside the vault"),
            None => return deny(&request, "the asset worker stopped"),
        };
        match served {
            Served::File => send(&request, &path),
            Served::Bytes(bytes, mime) => {
                let stream = gio::MemoryInputStream::from_bytes(&bytes);
                request.finish(&stream, bytes.len() as i64, Some(mime));
            }
        }
    });
}

/// Hand the file at `path` to WebKit, or fail the request with whatever stopped us.
fn send(request: &webkit6::URISchemeRequest, path: &Path) {
    match gio::File::for_path(path).read(gio::Cancellable::NONE) {
        Ok(stream) => {
            let size = std::fs::metadata(path).map_or(-1, |m| m.len() as i64);
            let mime = gio::content_type_guess(Some(path), None).0;
            request.finish(&stream, size, Some(&mime));
        }
        Err(e) => deny(request, &e.to_string()),
    }
}

fn deny(request: &webkit6::URISchemeRequest, what: &str) {
    let mut error = glib::Error::new(gio::IOErrorEnum::PermissionDenied, what);
    request.finish_error(&mut error);
}

/// A note's asset path -> a real file on this machine, or `None`.
///
/// Containment is split now that the vault may be on another machine. The lexical half is here and
/// unconditional: an empty, absolute or `..`-escaping `rel` never reaches the resolver, which is
/// what stops `![[../../../etc/passwd]]` however the resolver is written. The other half is the
/// resolver's, and has to be: only it knows the root, so only it can say whether the file it hands
/// back is still inside the vault once symlinks have been followed.
///
/// A `note` from outside every vault, keyed by its absolute path, has no resolver to ask: its own
/// folder is the root, under the same two rules ([`beside`]). Its page is based at that folder's
/// path ([`base_uri`]), so what it links relatively arrives as a path under the folder, which is
/// taken back to one relative to it, and a `![[name]]` arrives bare.
fn resolve_asset(resolve: &Resolve, note: &str, rel: &str) -> Option<(String, PathBuf)> {
    let folder = Path::new(note)
        .parent()
        .filter(|_| Path::new(note).is_absolute());
    let rel = match folder.and_then(|dir| Path::new(rel).strip_prefix(dir).ok()) {
        Some(inside) => inside.to_str()?,
        None => rel,
    };
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return None;
    }
    // Never more `..` than there are directories to climb back out of.
    let mut depth = 0usize;
    for part in Path::new(rel).components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return None,
        }
    }
    match folder {
        Some(dir) => beside(dir, rel),
        None => resolve(rel),
    }
}

/// `rel` in the folder `dir` as a vault holds a file: the file it names once symlinks are followed,
/// while that is still under `dir`, keyed by its path as a loose tab is.
fn beside(dir: &Path, rel: &str) -> Option<(String, PathBuf)> {
    let root = dir.canonicalize().ok()?;
    let path = root.join(rel).canonicalize().ok()?;
    path.starts_with(&root)
        .then(|| (path.to_string_lossy().into_owned(), path))
}

/// The file on this machine an `accent://file/` address on `note`'s page names, held to the vault
/// as every request the page makes is.
pub(crate) fn asset(resolve: &Resolve, note: &str, uri: &str) -> Option<PathBuf> {
    asset_of(resolve, note, uri).map(|(_, path)| path)
}

/// [`asset`] with its key.
pub(crate) fn asset_of(resolve: &Resolve, note: &str, uri: &str) -> Option<(String, PathBuf)> {
    let Some(("file", rel)) = accent_uri(uri) else {
        return None;
    };
    resolve_asset(resolve, note, &rel)
}

/// The page an embedded diagram's address names (`![[x.drawio#Page]]` asks for
/// `x.drawio?page=Page`), percent-decoded; `None` for its first page or any other address.
pub(crate) fn diagram_page(uri: &str) -> Option<String> {
    let query = uri.split_once('?')?.1;
    let query = query.split_once('#').map_or(query, |(q, _)| q);
    query
        .split('&')
        .find_map(|p| p.strip_prefix("page="))
        .map(percent_decode)
}

/// Whether an `accent://file/` address names a diagram, which an export draws rather than
/// copies.
pub(crate) fn is_diagram(uri: &str) -> bool {
    matches!(accent_uri(uri), Some(("file", rel)) if accent_core::path::is_diagram(&rel))
}

/// Split `accent://<host>/<path>` into host and percent-decoded path, dropping `?query` and
/// `#fragment`. `None` for anything that is not an `accent:` URI.
fn accent_uri(uri: &str) -> Option<(&str, String)> {
    let rest = uri.strip_prefix("accent://")?;
    let head = rest.split_once(['?', '#']).map_or(rest, |(h, _)| h);
    let (host, path) = head.split_once('/').unwrap_or((head, ""));
    Some((host, percent_decode(path)))
}

// -------------------------------------------------------------------------------------- document

/// Where relative links in `rel`'s markdown resolve from: the note's own directory.
fn base_uri(rel: &str) -> String {
    match parent_dir(rel) {
        "" => "accent://file/".to_string(),
        dir => format!(
            "accent://file/{}/",
            glib::Uri::escape_string(dir, Some("/"), false)
        ),
    }
}

/// The smallest document that will hold the fragment. All styling arrives as a user stylesheet, so
/// a re-render never re-sends the CSS.
fn document(body: &str) -> String {
    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>{body}</body></html>")
}

// ----------------------------------------------------------------------------------- stylesheet

fn css_rgba(c: gdk::RGBA) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "rgba({}, {}, {}, {:.2})",
        byte(c.red()),
        byte(c.green()),
        byte(c.blue()),
        c.alpha()
    )
}

/// [`theme_css`] for `fg` on `bg`, in the editor's font and the system accent.
fn stylesheet(fg: gdk::RGBA, bg: &str) -> String {
    // The editor's own font, not the GNOME document font: DESIGN.md's Typography section gives
    // prose Adwaita Mono at the *size* of the document font, and the two panes are meant to agree
    // by construction. Asked of `editor::default_font`, which is where that decision is made, so a
    // change there reaches the preview without a second edit.
    let font = pango::FontDescription::from_string(&crate::editor::default_font());
    let family = font
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(|| "Adwaita Mono".to_string());
    let size = match font.size() as f64 / pango::SCALE as f64 {
        pt if pt > 0.0 => pt,
        _ => 11.0,
    };
    let accent = adw::StyleManager::default().accent_color_rgba();
    theme_css(fg, bg, accent, &family, size)
}

/// The sheet a paper preview is given, for an export that carries it in the file: WebKit applies
/// it as a user stylesheet, which the page's own DOM does not hold.
pub(crate) fn paper_css() -> String {
    let (bg, fg) = theme::paper();
    stylesheet(fg, bg)
}

/// The foreground at `alpha`, as CSS: the same colour the editor dims with.
fn dim(c: gdk::RGBA, alpha: f32) -> String {
    css_rgba(crate::theme::at(c, alpha))
}

/// The whole preview look, derived from three values plus the document font. The alphas are the
/// ones `highlight.rs::restyle` gives the editor, so the two panes read as one app; so are a
/// conflict block's tints (`conflict::tints`), its sides boxed where the editor tints lines. A
/// caption is its marker line's tint laid over the page, opaque, as the editor lays that line:
/// left translucent, the side's own tint under it showed through and darkened it.
fn theme_css(fg: gdk::RGBA, bg: &str, accent: gdk::RGBA, family: &str, pt: f64) -> String {
    let (text, accent) = (css_rgba(fg), css_rgba(accent));
    let (surface, quote, rule) = (dim(fg, 0.07), dim(fg, 0.6), dim(fg, 0.15));
    let page = gdk::RGBA::parse(bg).unwrap_or(gdk::RGBA::WHITE);
    let tints: String = ["current", "base", "incoming"]
        .into_iter()
        .zip(crate::conflict::tints(fg, page))
        .map(|(side, (body, head))| {
            format!(
                ".conflict-{side} {{ background: {}; }}\n\
                 .conflict-{side} > .conflict-label {{ background: {}; }}\n",
                css_rgba(body),
                css_rgba(crate::theme::over(head, page))
            )
        })
        .collect();
    format!(
        "html {{ background: {bg}; color: {text}; font-family: \"{family}\", sans-serif; \
         font-size: {pt}pt; }}\n\
         body {{ max-width: {COLUMN_CH}ch; margin: 0 auto; padding: 24px 48px 96px; \
         line-height: 1.6; }}\n\
         /* Scroll-sync markers: the script scrolls the block around them, never the marker. */\n\
         span[data-line] {{ display: none; }}\n\
         h1, h2, h3, h4, h5, h6 {{ font-weight: 700; line-height: 1.25; margin: 1.2em 0 0.4em; }}\n\
         h1 {{ font-size: 1.6rem; }}\n\
         h2 {{ font-size: 1.4rem; }}\n\
         h3 {{ font-size: 1.2rem; }}\n\
         h4 {{ font-size: 1.1rem; }}\n\
         h5, h6 {{ font-size: 1rem; }}\n\
         p {{ margin: 0.8em 0; }}\n\
         ul, ol {{ padding-left: 1.4em; }}\n\
         /* A task item's checkbox is its bullet: it takes the marker's place in the gutter, so\n\
            its text lines up with a plain item's beside it. */\n\
         li.task-list-item {{ list-style: none; }}\n\
         li.task-list-item input {{ margin: 0 0 0 -1.4em; }}\n\
         a {{ color: {accent}; text-decoration: underline; }}\n\
         img {{ max-width: 100%; height: auto; }}\n\
         /* A display formula's box is its ink: unlike a line of prose it carries none of the\n\
            half-leading `line-height: 1.6` gives, so two of them would sit closer together than\n\
            two paragraphs, and two in one paragraph would touch. 1.2em is what a heading takes\n\
            above itself, and it is in `em` so it follows the document font. */\n\
         math[display=\"block\"] {{ margin: 1.2em 0; }}\n\
         code, pre, .math {{ font-family: monospace; font-size: 0.92em; }}\n\
         code {{ background: {surface}; border-radius: 4px; padding: 0.1em 0.3em; }}\n\
         pre {{ background: {surface}; border-radius: 6px; padding: 12px; overflow-x: auto; }}\n\
         pre code {{ background: none; padding: 0; }}\n\
         blockquote {{ margin: 1em 0; padding-left: 12px; border-left: 3px solid {accent}; \
         color: {quote}; font-style: italic; }}\n\
         hr {{ border: 0; border-top: 1px solid {rule}; margin: 24px 0; }}\n\
         table {{ border-collapse: collapse; }}\n\
         th, td {{ border: 1px solid {rule}; padding: 6px 12px; }}\n\
         th {{ font-weight: 700; }}\n\
         /* A conflict block: its sides stacked in one rounded box, each under a caption naming\n\
            it. A side is a flow root, so its paragraphs' margins stay inside its tint. */\n\
         .conflict {{ margin: 1em 0; border-radius: 6px; overflow: auto; }}\n\
         .conflict > div {{ display: flow-root; padding: 0 12px; }}\n\
         .conflict-label {{ margin: 0 -12px; padding: 2px 12px; font-size: 0.85em; \
         font-weight: 700; }}\n\
         {tints}\
         /* On paper the page's margins are the print settings', a line of code has no sideways\n\
            scroll to hide in, and a heading or a figure is not cut from what it belongs to. */\n\
         @media print {{\n\
         body {{ max-width: none; padding: 0; }}\n\
         pre {{ white-space: pre-wrap; overflow-wrap: anywhere; }}\n\
         h1, h2, h3, h4, h5, h6 {{ break-after: avoid; }}\n\
         pre, table, img, svg, .conflict, math[display=\"block\"] {{ break-inside: avoid; }}\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FG: gdk::RGBA = gdk::RGBA::new(0.0, 0.0, 0.0, 0.8);
    const ACCENT: gdk::RGBA = gdk::RGBA::new(1.0, 0.0, 0.0, 1.0);

    /// Every `#rrggbb`-looking run in `css`.
    fn hex_literals(css: &str) -> Vec<&str> {
        css.match_indices('#')
            .map(|(i, _)| {
                let n = css[i + 1..]
                    .chars()
                    .take_while(char::is_ascii_hexdigit)
                    .count();
                &css[i..i + 1 + n]
            })
            .collect()
    }

    /// A scratch directory of our own, since the crate has no tempdir dependency.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("accent-preview-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn theme_css_paints_links_with_the_accent() {
        let css = theme_css(FG, theme::view_bg(true), ACCENT, "Cantarell", 11.0);
        assert!(
            css.contains("a { color: rgba(255, 0, 0, 1.00); text-decoration: underline; }"),
            "{css}"
        );
        assert!(css.contains("border-left: 3px solid rgba(255, 0, 0, 1.00)"));
    }

    /// A conflict block's boxes are the editor's tints of its sides, their captions the tints of
    /// the marker lines over the page, so the two panes show one block.
    #[test]
    fn theme_css_tints_a_conflict_as_the_editor_does() {
        let css = theme_css(FG, theme::view_bg(false), ACCENT, "Cantarell", 11.0);
        let page = gdk::RGBA::parse(theme::view_bg(false)).unwrap();
        for (side, (body, head)) in ["current", "base", "incoming"]
            .into_iter()
            .zip(crate::conflict::tints(FG, page))
        {
            let rule = format!(".conflict-{side} {{ background: {}; }}", css_rgba(body));
            assert!(css.contains(&rule), "{rule} in {css}");
            let rule = format!(
                ".conflict-{side} > .conflict-label {{ background: {}; }}",
                css_rgba(theme::over(head, page))
            );
            assert!(css.contains(&rule), "{rule} in {css}");
        }
    }

    #[test]
    fn theme_css_writes_no_hex_but_the_background() {
        for bg in [theme::view_bg(false), theme::view_bg(true)] {
            let css = theme_css(FG, bg, ACCENT, "Cantarell", 11.0);
            assert_eq!(hex_literals(&css), vec![bg], "stray hex with {bg}");
        }
        // Paper is white whatever the theme on screen.
        let (bg, ink) = theme::paper();
        assert_eq!(bg, theme::view_bg(false));
        let css = theme_css(ink, bg, ACCENT, "Cantarell", 11.0);
        assert_eq!(hex_literals(&css), vec![bg]);
    }

    /// On paper the page is the paper's width, a long code line wraps rather than running off it,
    /// and no heading is left at the foot of a page away from what it heads.
    #[test]
    fn theme_css_lays_the_note_out_for_print() {
        let css = theme_css(FG, theme::view_bg(false), ACCENT, "Cantarell", 11.0);
        let print = css.split_once("@media print").expect("a print block").1;
        for rule in [
            "body { max-width: none; padding: 0; }",
            "pre { white-space: pre-wrap; overflow-wrap: anywhere; }",
            "h1, h2, h3, h4, h5, h6 { break-after: avoid; }",
            "pre, table, img, svg, .conflict, math[display=\"block\"] { break-inside: avoid; }",
        ] {
            assert!(print.contains(rule), "{rule} in {print}");
        }
    }

    #[test]
    fn theme_css_follows_its_inputs() {
        let (light, dark) = (theme::view_bg(false), theme::view_bg(true));
        let base = theme_css(FG, light, ACCENT, "Cantarell", 11.0);
        let teal = gdk::RGBA::new(0.0, 0.5, 0.5, 1.0);
        assert_ne!(base, theme_css(FG, dark, ACCENT, "Cantarell", 11.0));
        assert_ne!(base, theme_css(FG, light, teal, "Cantarell", 11.0));
        assert_ne!(base, theme_css(FG, light, ACCENT, "Inter", 11.0));
        assert_ne!(base, theme_css(FG, light, ACCENT, "Cantarell", 13.0));
        assert!(theme_css(FG, light, ACCENT, "Inter", 13.0).contains("\"Inter\""));
    }

    /// A resolver of the shape a caller that owns the root passes in: a join that canonicalises,
    /// so nothing under the root can lead out of it.
    fn vault_resolver(root: PathBuf) -> impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync {
        move |rel| {
            let root = root.canonicalize().ok()?;
            let path = root.join(rel).canonicalize().ok()?;
            path.starts_with(&root).then(|| (rel.to_string(), path))
        }
    }

    #[test]
    fn resolve_asset_accepts_a_path_inside_the_vault() {
        let root = scratch("inside");
        std::fs::create_dir(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments/img.png"), b"x").unwrap();
        let resolve = vault_resolver(root.clone());
        assert_eq!(
            resolve_asset(&resolve, "n.md", "attachments/img.png"),
            Some((
                "attachments/img.png".to_string(),
                root.canonicalize().unwrap().join("attachments/img.png")
            ))
        );
        // Nothing there is the resolver's `None`, and reaches the reader as the same refusal.
        assert_eq!(resolve_asset(&resolve, "n.md", "missing.png"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_asset_rejects_traversal_and_absolute_paths() {
        // A resolver that hands back whatever it is asked for, so a `None` below can only have
        // come from the check here — which is the point: it holds for any resolver.
        let naive = |rel: &str| Some((rel.to_string(), PathBuf::from(rel)));
        for rel in [
            "../../../../etc/passwd",
            "notes/../../etc/passwd",
            "/etc/passwd",
            "",
        ] {
            assert_eq!(resolve_asset(&naive, "n.md", rel), None, "{rel}");
        }
    }

    #[test]
    fn resolve_asset_leaves_a_symlink_out_of_the_vault_to_the_resolver() {
        let root = scratch("symlink");
        let outside = scratch("symlink-target");
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();

        // Neither path is lexically wrong, so the check here passes them on; refusing them takes
        // the root, which only the resolver has.
        let resolve = vault_resolver(root.clone());
        assert_eq!(resolve_asset(&resolve, "n.md", "escape.txt"), None);
        assert_eq!(resolve_asset(&resolve, "n.md", "out/secret.txt"), None);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// A note outside every vault reads its images beside it: its folder stands in for the vault
    /// root, so a relative link or a bare `![[name]]` reaches a file in it or under it, and nothing
    /// above it, named by its path or through a symlink out of it.
    #[test]
    fn a_loose_note_reads_its_images_beside_it() {
        let root = scratch("loose").canonicalize().unwrap();
        let dir = root.join("n");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for file in ["n/img.png", "n/sub/b.png", "above.png"] {
            std::fs::write(root.join(file), b"x").unwrap();
        }
        std::os::unix::fs::symlink(root.join("above.png"), dir.join("out.png")).unwrap();
        let note = dir.join("note.md");
        // A loose note never asks the vault's resolver.
        let vault = |_: &str| -> Option<(String, PathBuf)> { unreachable!() };
        let found = |rel: &Path| resolve_asset(&vault, note.to_str()?, rel.to_str()?);
        let img = dir.join("img.png");
        // `![](img.png)` as the page based at the folder asks for it, then `![[img.png]]`.
        assert_eq!(
            found(&img),
            Some((img.to_string_lossy().into_owned(), img.clone()))
        );
        assert_eq!(found(Path::new("img.png")).map(|(_, p)| p), Some(img));
        let deeper = dir.join("sub/b.png");
        assert_eq!(found(&deeper).map(|(_, p)| p), Some(deeper));
        // `![](../above.png)`, the page's way and a wikilink's, and a symlink out of the folder.
        assert_eq!(found(&root.join("above.png")), None);
        assert_eq!(found(Path::new("../above.png")), None);
        assert_eq!(found(Path::new("out.png")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn accent_uri_decodes_the_target_and_drops_the_anchor() {
        assert_eq!(
            accent_uri("accent://open/Deep%20Work#Section%20One"),
            Some(("open", "Deep Work".to_string()))
        );
        assert_eq!(
            accent_uri("accent://open/Notes-PHD/Deep%20Work.md"),
            Some(("open", "Notes-PHD/Deep Work.md".to_string()))
        );
        assert_eq!(
            accent_uri("accent://file/img%20a.png?v=1"),
            Some(("file", "img a.png".to_string()))
        );
        // A malformed escape stays as written rather than eating the next character.
        assert_eq!(
            accent_uri("accent://open/100%25%zz"),
            Some(("open", "100%%zz".to_string()))
        );
        assert_eq!(accent_uri("https://example.com/x"), None);
    }

    /// A click hands the system what Go to Definition would, and nothing the page could run or
    /// read the disk with; without a click nothing leaves at all.
    #[test]
    fn an_embedded_diagram_names_its_page() {
        let uri = "accent://file/Figures/flow.drawio?page=Page%202";
        assert!(is_diagram(uri));
        assert_eq!(diagram_page(uri).as_deref(), Some("Page 2"));
        assert_eq!(diagram_page("accent://file/flow.drawio"), None);
        assert!(!is_diagram("accent://file/flow.png"));
    }

    #[test]
    fn a_click_launches_what_the_editor_would_and_nothing_else() {
        for url in ["https://e.org", "mailto:a@b.c", "tel:+123", "sms:+123"] {
            assert!(launches(url, true), "{url}");
            assert!(!launches(url, false), "{url} without a click");
        }
        for not in [
            "javascript:alert(1)",
            "javascript://%0aalert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
        ] {
            assert!(!launches(not, true), "{not}");
        }
    }

    #[test]
    fn an_anchor_into_the_page_on_screen_stays_on_it() {
        let page = "accent://file/Notes/";
        assert!(same_page("accent://file/Notes/#intro", page));
        // After the first jump the view's own URI carries the fragment it scrolled to.
        assert!(same_page(
            "accent://file/Notes/#outro",
            "accent://file/Notes/#intro"
        ));
        assert!(!same_page("accent://file/Notes/Other.md#intro", page));
        // No fragment is a load of the directory, not a place on the page.
        assert!(!same_page(page, page));
    }

    #[test]
    fn matches_label_says_where_the_reader_is() {
        assert_eq!(matches_label(0, 0), "No results");
        assert_eq!(matches_label(1, 1), "1 of 1");
        assert_eq!(matches_label(3, 12), "3 of 12");
        // Before WebKit has answered, or after a step with nothing to step through.
        assert_eq!(matches_label(0, 12), "12 matches");
        // Past WebKit's own ceiling the total is a floor, so no position is claimed.
        assert_eq!(matches_label(1, FIND_LIMIT), "500+ matches");
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        assert_eq!(stepped(1, 3, true), 2);
        assert_eq!(stepped(3, 3, true), 1);
        assert_eq!(stepped(2, 3, false), 1);
        assert_eq!(stepped(1, 3, false), 3);
        // Nothing found: both directions stay at nothing.
        assert_eq!(stepped(0, 0, true), 0);
        assert_eq!(stepped(0, 0, false), 0);
    }

    #[test]
    fn a_page_waits_for_the_network_filter_and_never_loads_without_it() {
        let page = |body: &str| ("a.md".to_string(), body.to_string());
        let mut gate = Gate::Waiting(None);
        assert_eq!(gate.pass(page("one")), None);
        assert_eq!(gate.pass(page("two")), None);
        // Only the latest render goes, once the filter is on the view.
        assert_eq!(gate.settle(Ok(())), Some(page("two")));
        assert_eq!(gate.pass(page("three")), Some(page("three")));

        let mut gate = Gate::Waiting(None);
        gate.pass(page(r#"<img src="http://example.com/x.png">"#));
        let (rel, shown) = gate.settle(Err("no store".into())).unwrap();
        assert_eq!(rel, "a.md");
        assert!(shown.contains("no store") && !shown.contains("example.com"));
        let (_, shown) = gate.pass(page("<p>four</p>")).unwrap();
        assert!(!shown.contains("four"));
    }

    #[test]
    fn base_uri_is_the_notes_own_directory() {
        assert_eq!(base_uri("Index.md"), "accent://file/");
        assert_eq!(base_uri("Notes/Deep Work.md"), "accent://file/Notes/");
        assert_eq!(base_uri("a b/c d/Note.md"), "accent://file/a%20b/c%20d/");
    }
}
