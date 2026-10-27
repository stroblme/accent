//! Read-only WebKitGTK markdown preview fed by `markdown::to_html`.
//!
//! Synced notes are untrusted: an ephemeral network session, disabled page JavaScript,
//! load-denying filter and vault-only `accent:` scheme restrict their access.
//!
//! Generated CSS uses the editor's foreground, background, accent and font because WebKit
//! cannot read GTK CSS variables. This keeps both views consistent across themes.

use crate::assets::{Resolve, diagram_page, resolve_asset};
use crate::export::css::{column, stylesheet};
use crate::look::{self, Look, Served};
use crate::theme;
use accent_core::config::Config;
use accent_core::markdown::{self, Click, accent_file, base_uri};
use accent_core::search::Options;
use gtk::{gdk, gio, glib};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use webkit6::prelude::*;

/// WebKit find options: wrap, optional case matching and word starts for whole-word mode.
/// WebKit has no regex search; queries remain literal, and foo can match food at word starts.
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

/// Idle lifetime for unused WebKit views and their 60–120 MB processes.
/// Keep short toggles and successive formulas warm; ACCENT_BENCH_WEBIDLE uses two seconds.
pub fn idle_for() -> std::time::Duration {
    #[cfg(feature = "bench")]
    if std::env::var_os("ACCENT_BENCH_WEBIDLE").is_some() {
        return std::time::Duration::from_secs(2);
    }
    std::time::Duration::from_secs(5 * 60)
}

/// Preview-console tracing target, selectable through RUST_LOG=accent::preview=debug.
const PREVIEW: &str = "accent::preview";

/// The name the page posts its console through; spelled again inside [`CONSOLE_SCRIPT`], which is
/// a JavaScript literal and cannot interpolate a Rust constant.
const LOG_HANDLER: &str = "accentLog";

/// Block all loads except accent-scheme assets.
/// Navigation policy alone misses subresources such as tracking pixels in note HTML.
const BLOCK_NETWORK: &str = concat!(
    r#"[{"trigger":{"url-filter":".*"},"action":{"type":"block"}},"#,
    r#"{"trigger":{"url-filter":"^accent:"},"action":{"type":"ignore-previous-rules"}}]"#
);

/// Scroll-sync script targets marker-containing blocks because to_html source-line markers are hidden.
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

/// Patch top-level rendered blocks while retaining unchanged blocks and their UI state.
///
/// Compare HTML without source-line markers so insertions above a block do not replace it;
/// update retained markers from the new render. Capture blocks at DOMContentLoaded before
/// Mermaid replaces fences, and leave script-added nodes intact.
///
/// Return whether the selection precedes all changes, allowing the find counter to retain
/// its current match position.
const PATCH_SCRIPT: &str = r#"
(function () {
  var keys = new Map(), joined = new Set();
  var key = function (node) {
    return node.nodeType === 1 ? node.outerHTML.replace(/ data-line="\d+"/g, '')
                               : node.nodeType + ':' + node.nodeValue;
  };
  var marks = function (nodes) {
    var out = [];
    nodes.forEach(function (n) {
      if (n.nodeType !== 1) { return; }
      if (n.matches('[data-line]')) { out.push(n); }
      out.push.apply(out, n.querySelectorAll('[data-line]'));
    });
    return out;
  };
  document.addEventListener('DOMContentLoaded', function () {
    document.body.childNodes.forEach(function (n) { keys.set(n, key(n)); });
  });
  window.__accentHandOver = function (fence, mark, drawing) {
    if (!keys.has(fence)) { return; }
    keys.set(mark || drawing, keys.get(fence));
    keys.delete(fence);
    if (mark) { joined.add(drawing); }
  };
  window.__accentPatch = function (html) {
    var runs = [];
    document.body.childNodes.forEach(function (n) {
      if (keys.has(n)) { runs.push({ key: keys.get(n), nodes: [n] }); }
      else if (joined.has(n) && runs.length) { runs[runs.length - 1].nodes.push(n); }
    });
    var t = document.createElement('template');
    t.innerHTML = html;
    var next = Array.from(t.content.childNodes), fresh = next.map(key);
    var a = 0, b = 0;
    while (a < runs.length && a < next.length && runs[a].key === fresh[a]) { a++; }
    while (b < runs.length - a && b < next.length - a &&
           runs[runs.length - 1 - b].key === fresh[next.length - 1 - b]) { b++; }
    var sel = getSelection(), first = a < runs.length ? runs[a].nodes[0] : null;
    var before = sel.rangeCount > 0 && !sel.isCollapsed &&
                 (!first || sel.getRangeAt(0).comparePoint(first, 0) === 1);
    var end = runs.length - b, at = end < runs.length ? runs[end].nodes[0] : null;
    for (var i = a; i < end; i++) {
      runs[i].nodes.forEach(function (n) { keys.delete(n); joined.delete(n); n.remove(); });
    }
    for (var j = a; j < next.length - b; j++) {
      keys.set(next[j], fresh[j]);
      document.body.insertBefore(next[j], at);
    }
    for (var k = 1; k <= b; k++) {
      var page = marks(runs[runs.length - k].nodes), want = marks([next[next.length - k]]);
      for (var m = 0; m < page.length && m < want.length; m++) {
        var line = want[m].getAttribute('data-line');
        if (page[m].getAttribute('data-line') !== line) { page[m].setAttribute('data-line', line); }
      }
    }
    if (window.__accentDraw) { window.__accentDraw(); }
    return before;
  };
})();
"#;

/// Forward JavaScript errors through script messages because web-process stdout is unavailable.
/// Wrap console.error while retaining Web Inspector output.
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

/// Vendored Mermaid and bootstrap, injected only when the note contains diagrams.
///
/// The preview blocks non-`accent:` loads, so diagrams must render offline without runtime
/// fetches. Android shares `vendor/mermaid/bootstrap.js`; desktop adds theme selection,
/// scroll markers and rerendering for patched fences ([`PATCH_SCRIPT`]).
const MERMAID: &str = concat!(
    include_str!("../../../vendor/mermaid/mermaid.min.js"),
    "\n",
    include_str!("../../../vendor/mermaid/bootstrap.js"),
    r#"
window.__accentDraw = function () {
  var rgb = getComputedStyle(document.documentElement).backgroundColor.match(/\d+/g) || [255, 255, 255];
  var luma = (0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]) / 255;
  // Kept where a print or an export can wait for the diagrams before taking the page.
  window.__accentDrawn = accentDiagrams(luma < 0.5 ? 'dark' : 'neutral', function (fence, drawing) {
    // The block's scroll-sync marker sits inside the fence and has to outlive it.
    var mark = fence.querySelector('[data-line]');
    if (mark) { fence.parentElement.insertBefore(mark, fence); }
    __accentHandOver(fence, mark, drawing);
  });
};
window.__accentDraw();
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
    /// The page's share of the view in percent, `None` for all of it ([`Preview::set_column`]).
    column: Cell<Option<u32>>,
    /// [`MERMAID`] while a note with a diagram is shown, `None` otherwise.
    mermaid: RefCell<Option<webkit6::UserScript>>,
    /// Whether a page may load yet; see [`Gate`].
    gate: RefCell<Gate>,
    /// Render generation so slow older workers cannot overwrite newer requested pages.
    renders: Cell<u64>,
    /// Whether the page up is the latest render's.
    loaded: Cell<bool>,
    /// The note whose page has finished loading, which a render of it patches ([`Inner::put`]);
    /// `None` while a page loads, and once WebKit's process or its images are gone.
    shown: RefCell<Option<String>>,
    /// A line asked for while the page was still loading.
    pending: Cell<Option<u32>>,
    /// What the find bar is looking for, and under which of its toggles, kept because a new
    /// page drops WebKit's find, and a patch may replace the blocks its matches are in.
    query: RefCell<Option<String>>,
    options: Cell<Options>,
    /// Find total and locally tracked position, 1-based or 0 for none.
    /// WebKit reports totals only; fresh searches reset position and stepping mirrors its traversal.
    total: Cell<u32>,
    at: Cell<u32>,
    /// Where the readout goes, called with the label rather than the numbers so the bar does not
    /// have to know how a preview counts.
    report: RefCell<Option<Report>>,
    /// Whether the one find-count handler is installed, avoiding double counting when reporters change.
    counting: Cell<bool>,
    /// Whether the count on its way is of a search from the reader's match, which keeps `at`,
    /// rather than from the top ([`Inner::refind`]).
    from_match: Cell<bool>,
    /// Whether WebKit's process died under the page since a page last finished loading; see
    /// [`Preview::connect_lost`].
    lost: Cell<bool>,
}

impl Inner {
    /// Patch `rel`'s existing page when scripts match, otherwise load anew.
    /// Only loads update base URI and scripts; [`versioned`] gives changed assets new cache addresses.
    fn put(self: &Rc<Self>, rel: &str, body: &str) {
        let base = base_uri(rel);
        let body = versioned(body, &base, &self.assets.versions.borrow());
        let diagrams = body.contains("language-mermaid");
        if self.shown.borrow().as_deref() == Some(rel)
            && diagrams == self.mermaid.borrow().is_some()
            && !self.view.is_loading()
        {
            return self.patch(&body);
        }
        *self.assets.note.borrow_mut() = rel.to_string();
        self.shown.take();
        self.set_mermaid(diagrams);
        self.loaded.set(false);
        self.view.load_html(&document(&body), Some(&base));
    }

    /// Patch body blocks through [`PATCH_SCRIPT`] without changing loading or network filtering.
    /// Only uncached images in new blocks are requested.
    fn patch(self: &Rc<Self>, body: &str) {
        let render = self.renders.get();
        let args = glib::VariantDict::new(None);
        args.insert("html", body);
        let inner = Rc::downgrade(self);
        self.view.call_async_javascript_function(
            "return __accentPatch(html);",
            Some(&args.end()),
            None,
            None,
            gio::Cancellable::NONE,
            move |result| {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let kept = result.map(|v| v.to_boolean()).unwrap_or_else(|e| {
                    // The page is the last render's: the next one loads a new page.
                    tracing::warn!(target: PREVIEW, "cannot patch the page: {e}");
                    inner.shown.take();
                    false
                });
                if inner.renders.get() == render {
                    inner.settle(kept);
                }
            },
        );
    }

    /// The latest render is on the page: scroll where it was asked to, and search it again,
    /// from the reader's match if the selection on it was `kept` before every change.
    fn settle(&self, kept: bool) {
        self.loaded.set(true);
        if let Some(line) = self.pending.take() {
            self.scroll(line);
        }
        self.refind(kept);
    }

    /// The network filter is in, or could not be: load what waited for it.
    fn unlock(self: &Rc<Self>, filter: &Filter) {
        let filter = filter
            .as_ref()
            .map(|f| self.content.add_filter(f))
            .map_err(Clone::clone);
        let next = self.gate.borrow_mut().settle(filter);
        if let Some((rel, body)) = next {
            self.put(&rel, &body);
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

    /// Run the stored find query only when the page is loaded; rerun from the load handler.
    ///
    /// WebKit silently reports no matches while loading. `from_match` preserves the selected
    /// match after patches that leave earlier blocks unchanged, recounting only the total.
    fn refind(&self, from_match: bool) {
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
        // which is what lets the counter say "1 of 12" and mean it; collapsing it to its start
        // lands on the match it holds. Fire and forget: the script and the find both go to the
        // web process over the same connection, in this order.
        let selection = match from_match {
            true => "window.getSelection().collapseToStart()",
            false => "window.getSelection().removeAllRanges()",
        };
        self.from_match.set(from_match);
        self.view
            .evaluate_javascript(selection, None, None, gio::Cancellable::NONE, report_js);
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

    /// Install Mermaid only for notes containing diagrams to avoid parsing its 3.4 MB script unnecessarily.
    /// Removing it retains SCROLL_SCRIPT.
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

/// Log evaluate_javascript results where errors retain detail.
/// WebKit scrubs injected-script failures to Script error in window.onerror.
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

/// Format 1-based match `at` and total, with 0 for no selection.
/// Totals reaching [`FIND_LIMIT`] are lower bounds, so use capped wording rather than false exact positions.
fn matches_label(at: u32, total: u32) -> String {
    match (at, total) {
        (_, 0) => "No results".to_string(),
        (_, n) if n >= FIND_LIMIT => format!("{n}+ matches"),
        (0, n) => format!("{n} matches"),
        (at, n) => format!("{at} of {n}"),
    }
}

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
    /// Served keys and request paths since the memory cache was cleared, used to invalidate stale WebKit responses.
    served: RefCell<HashMap<String, HashSet<String>>>,
    /// The request paths last served an animation, over which the menu's Invert is greyed out.
    animations: RefCell<HashSet<String>>,
    /// How many times the file a path names changed under the page, which asks for it with `v=`
    /// that many ([`versioned`]).
    versions: RefCell<HashMap<String, u32>>,
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
    /// Build a preview with asset resolution and navigation callbacks.
    ///
    /// `resolve` maps vault-relative assets to local files under [`resolve_asset`]'s containment
    /// contract. `inverted` stores image inversions and `web` supplies embedded web diagrams.
    /// `on_open` receives a written wikilink target or markdown vault path plus any anchor;
    /// `on_tag` receives clicked tags, and `on_invert` receives image keys.
    ///
    /// Each preview owns a WebContext and therefore a WebKit process group.
    pub fn new(
        resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        inverted: Keys,
        web: Keys,
        on_open: impl Fn(&str) + 'static,
        on_tag: impl Fn(&str) + 'static,
        on_invert: impl Fn(&str) + 'static,
    ) -> Preview {
        Self::build(
            Arc::new(resolve),
            inverted,
            web,
            on_open,
            on_tag,
            on_invert,
            false,
        )
    }

    /// Build an unseen print/export preview: white paper, original images and neutral Mermaid diagrams.
    /// Disable navigation so paper preparation cannot follow links.
    pub fn for_paper(resolve: Arc<Resolve>, web: Keys) -> Preview {
        Self::build(resolve, Rc::default(), web, |_| {}, |_| {}, |_| {}, true)
    }

    fn build(
        resolve: Arc<Resolve>,
        inverted: Keys,
        web: Keys,
        on_open: impl Fn(&str) + 'static,
        on_tag: impl Fn(&str) + 'static,
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
            animations: RefCell::default(),
            versions: RefCell::default(),
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
        for script in [SCROLL_SCRIPT, PATCH_SCRIPT] {
            content.add_script(&webkit6::UserScript::new(
                script,
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserScriptInjectionTime::Start,
                &[],
                &[],
            ));
        }
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
            column: Cell::new(None),
            mermaid: RefCell::new(None),
            gate: RefCell::new(Gate::Waiting(None)),
            renders: Cell::new(0),
            loaded: Cell::new(false),
            shown: RefCell::new(None),
            pending: Cell::new(None),
            query: RefCell::new(None),
            options: Cell::new(Options::default()),
            total: Cell::new(0),
            at: Cell::new(0),
            report: RefCell::new(None),
            counting: Cell::new(false),
            from_match: Cell::new(false),
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
                    inner.lost.set(false);
                    *inner.shown.borrow_mut() = Some(inner.assets.note.borrow().clone());
                    inner.settle(false);
                }
            }
        ));
        // A new process has none of the page: the next render loads one.
        inner.view.connect_web_process_terminated(glib::clone!(
            #[weak]
            inner,
            move |_, _| {
                inner.shown.take();
            }
        ));

        inner
            .view
            .connect_decide_policy(move |view, decision, kind| {
                decide(view, decision, kind, &on_open, &on_tag)
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

    /// Render `text` with relative links based at `rel`.
    ///
    /// Same-note renders patch unchanged blocks to preserve reading and selection state; other
    /// notes start at the top for the caller to restore via [`Preview::scroll_to_line`]. Workers
    /// render HTML, only the latest result lands, and [`Gate`] waits for the network filter.
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
                inner.put(&rel, &body);
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

    /// Clear image cache, then call `then` to reload rather than patch.
    /// A fresh page requests newly styled images instead of retaining old block content.
    pub fn forget_images(&self, then: impl FnOnce() + 'static) {
        self.inner.assets.served.borrow_mut().clear();
        self.inner.shown.take();
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

    /// Invalidate served `key` or folder descendants using [`versioned`] asset addresses.
    /// Return whether any were served; unrelated images keep their cache entries.
    pub fn forget_image(&self, key: &str) -> bool {
        let folder = format!("{key}/");
        let mut versions = self.inner.assets.versions.borrow_mut();
        let mut found = false;
        self.inner
            .assets
            .served
            .borrow_mut()
            .retain(|served, paths| {
                if served != key && !served.starts_with(&folder) {
                    return true;
                }
                for path in paths.drain() {
                    *versions.entry(path).or_default() += 1;
                }
                found = true;
                false
            });
        found
    }

    /// How many requests the page has made so far.
    #[cfg(feature = "bench")]
    pub fn requests(&self) -> u32 {
        self.inner.assets.requests.get()
    }

    /// Whether the page up is the latest render's, for a drill timing the renders.
    #[cfg(feature = "bench")]
    pub fn settled(&self) -> bool {
        self.inner.loaded.get()
    }

    /// The web view, for a drill that reads the page itself.
    pub fn view(&self) -> &webkit6::WebView {
        &self.inner.view
    }

    /// Call `f` once after web-process loss to render again.
    /// Reset only after successful page load, avoiding crash loops for notes that fail while loading.
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
        // WebKit's zoom scales the floor's pixels but not a percentage of the view, so the share
        // carries the zoom itself, as the editor's column does.
        let share = inner
            .column
            .get()
            .map(|c| f64::from(c) * inner.view.zoom_level());
        let css = stylesheet(fg, bg, share);

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
    /// never touches it, so a stylesheet rebuild cannot undo it; the page's cap is scaled by it.
    pub fn set_zoom(&self, zoom: f64) {
        self.inner.view.set_zoom_level(zoom);
        Self::apply_style(&self.inner);
    }

    /// The page's width as Preferences set it: the editor's Column Width, or the whole view.
    pub fn set_column(&self, config: &Config) {
        let column = column(config);
        if self.inner.column.replace(column) != column {
            Self::apply_style(&self.inner);
        }
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
        // A page being patched is up to search; one loading is searched once it has loaded.
        if self.inner.shown.borrow().is_some() {
            self.inner.refind(false);
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
                // A search from the top lands on the first match whenever there is one; one from
                // the reader's match stays on it.
                let at = match inner.from_match.replace(false) {
                    true => inner.at.get().min(count),
                    false => (count > 0) as u32,
                };
                inner.at.set(at);
                inner.say();
            }
        ));
    }
}

// ------------------------------------------------------------------------------------- console

/// Forward page JavaScript errors to the application log.
///
/// Web-process stdout does not reliably reach this process. Uncaught errors use `warn!`
/// because features may fail; library console warnings and errors use `debug!`.
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

/// Provide the shared compiled filter to `then` before any file-backed view loads.
/// WebKit compiles asynchronously with disk storage; later views reuse the cached result.
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

/// Gate page loading on successful network-filter installation.
/// Retain the latest pending page; failed installation refuses notes entirely.
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
    /// Load `page` when filtering is ready, show refusal after failure, or replace the latest waiting page.
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

/// Route a navigation: links into the vault and tags back to the app, a web, mail, phone or
/// message link to the system, our own document load and an anchor into it through, everything
/// else nowhere.
fn decide(
    view: &webkit6::WebView,
    decision: &webkit6::PolicyDecision,
    kind: webkit6::PolicyDecisionType,
    on_open: &impl Fn(&str),
    on_tag: &impl Fn(&str),
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

    // Our own `load_html`, which arrives as the base URI and never as a click.
    if accent_file(&uri).is_some()
        && action.navigation_type() != webkit6::NavigationType::LinkClicked
    {
        return false;
    }
    match markdown::click(&uri, &view.uri().unwrap_or_default()) {
        // A `#tag`, which `to_html` makes a link to the tag's notes.
        Click::Tag(tag) => on_tag(&tag),
        // An in-note `[text](#slug)`: WebKit scrolls to the heading `to_html` gave that `id`.
        Click::Scroll => return false,
        // A wikilink, or a markdown link the base URI has already made a vault path.
        Click::Open(target) => on_open(&target),
        // Only for a click: a meta refresh or a redirect must not launch an application.
        Click::Leave if action.is_user_gesture() => {
            let parent = view.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(parent.as_ref(), gio::Cancellable::NONE, |result| {
                if let Err(e) = result {
                    tracing::warn!("preview: cannot open link: {e}");
                }
            });
        }
        Click::Leave | Click::Nowhere => tracing::debug!("preview: refused navigation to {uri}"),
    }
    decision.ignore();
    true
}

// ----------------------------------------------------------------------------------- image menu

/// Offer Invert Image Colours in the page's menu over an image of the vault's, handing
/// `on_invert` the image's key once the vault has named the file; over an animation, greyed out
/// and saying why, as Go to Source is without SyncTeX data.
fn image_menu(inner: &Inner, on_invert: impl Fn(&str) + 'static) {
    let action = gio::SimpleAction::new("invert-image", Some(glib::VariantTy::STRING));
    let animation = gio::SimpleAction::new("invert-animation", None);
    animation.set_enabled(false);
    let animations = inner.assets.clone();
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
        if let Some(rel) = uri.as_deref().and_then(accent_file) {
            menu.append(&webkit6::ContextMenuItem::new_separator());
            menu.append(&match animations.animations.borrow().contains(&rel) {
                true => webkit6::ContextMenuItem::from_gaction(
                    &animation,
                    &format!("Invert Image Colours {}", look::ANIMATED_LABEL),
                    None,
                ),
                false => webkit6::ContextMenuItem::from_gaction(
                    &action,
                    "Invert Image Colours",
                    Some(&rel.to_variant()),
                ),
            });
        }
        false
    });
}

// ----------------------------------------------------------------------------------- uri scheme

/// Serve or fail an `accent://file/<rel>` request, the preview's file-access boundary.
///
/// Retain the request while a worker resolves remote assets and applies [`look::serve`],
/// then finish on the main loop to avoid blocking it on downloads. Diagrams render on the
/// main loop for GTK access, then use [`look::serve_svg`].
fn serve(assets: &Rc<Assets>, request: &webkit6::URISchemeRequest) {
    #[cfg(feature = "bench")]
    assets.requests.set(assets.requests.get() + 1);
    let uri = request.uri().unwrap_or_default();
    let Some(rel) = accent_file(&uri) else {
        return deny(request, "not a vault file");
    };
    let asked = rel.clone();
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
            let animated = served.is_some() && look::animated(&path);
            Some((key, path, served, inverted, animated))
        });
        let answer = answer.await;
        if let Some(Some((key, .., animated))) = &answer {
            let mut animations = assets.animations.borrow_mut();
            match animated {
                true => animations.insert(asked.clone()),
                false => animations.remove(&asked),
            };
            let mut served = assets.served.borrow_mut();
            served.entry(key.clone()).or_default().insert(asked);
        }
        let (path, served) = match answer {
            Some(Some((_, path, Some(served), ..))) => (path, served),
            Some(Some((key, path, None, inverted, _))) => {
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

// -------------------------------------------------------------------------------------- document

/// Add v= versions to changed image addresses in `body` so WebKit fetches only invalidated files.
/// Match paths relative to `base`; unquoted addresses remain unchanged.
fn versioned<'a>(body: &'a str, base: &str, versions: &HashMap<String, u32>) -> Cow<'a, str> {
    if versions.is_empty() {
        return Cow::Borrowed(body);
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find("src=") {
        let (head, tail) = rest.split_at(at + "src=".len());
        out.push_str(head);
        rest = tail;
        let attribute = head[..at].ends_with(|c: char| c.is_ascii_whitespace());
        let Some(quote) = tail
            .chars()
            .next()
            .filter(|q| attribute && matches!(q, '"' | '\''))
        else {
            continue;
        };
        let Some(end) = tail[1..].find(quote) else {
            continue;
        };
        let src = &tail[1..1 + end];
        let Some(version) = version(src, base, versions) else {
            continue;
        };
        let (address, fragment) = src
            .split_once('#')
            .map_or((src, None), |(a, f)| (a, Some(f)));
        let join = if address.contains('?') { "&amp;" } else { "?" };
        out.push(quote);
        out.push_str(&format!("{address}{join}v={version}"));
        if let Some(fragment) = fragment {
            out.push('#');
            out.push_str(fragment);
        }
        rest = &tail[1 + end..];
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// How many times the file `src` names on a page based at `base` changed under the page, if it
/// did.
fn version(src: &str, base: &str, versions: &HashMap<String, u32>) -> Option<u32> {
    let src = src.replace("&amp;", "&");
    let uri = glib::Uri::resolve_relative(Some(base), &src, glib::UriFlags::NONE).ok()?;
    versions.get(&accent_file(&uri)?).copied()
}

/// The smallest document that will hold the fragment. All styling arrives as a user stylesheet, so
/// a re-render never re-sends the CSS.
fn document(body: &str) -> String {
    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>{body}</body></html>")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// An image whose file changed under the page is asked for at an address of its own, found by
    /// the path it names however the note writes it; every other address stays as it is.
    #[test]
    fn a_changed_image_gets_an_address_of_its_own() {
        let versions = HashMap::from([
            ("Notes/a b.png".to_string(), 2),
            ("x.drawio".to_string(), 1),
        ]);
        let base = "accent://file/Notes/";
        let body = concat!(
            r#"<img src="a%20b.png" alt="a" /><img src="accent://file/Notes/a%20b.png">"#,
            r#"<img src="accent://file/x.drawio?page=P"><img src='../Notes/a%20b.png#f'>"#,
            r#"<img data-src="a%20b.png"><img src="c.png"><p>src="a%20b.png"</p>"#,
        );
        assert_eq!(
            versioned(body, base, &versions),
            concat!(
                r#"<img src="a%20b.png?v=2" alt="a" /><img src="accent://file/Notes/a%20b.png?v=2">"#,
                r#"<img src="accent://file/x.drawio?page=P&amp;v=1"><img src='../Notes/a%20b.png?v=2#f'>"#,
                r#"<img data-src="a%20b.png"><img src="c.png"><p>src="a%20b.png"</p>"#,
            )
        );
        // Nothing changed is the page as `to_html` wrote it, not a copy.
        assert!(matches!(
            versioned(body, base, &HashMap::new()),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
