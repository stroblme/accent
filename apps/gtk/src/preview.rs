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

use crate::theme;
use accent_core::markdown::percent_decode;
use accent_core::path::parent_dir;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use webkit6::prelude::*;

/// Prose column width, in `ch`. DESIGN.md asks for 60 to 72 characters, and `ch` is the advance
/// of "0", which is wider than the average letter: measured on this pane, 56ch is 71 characters of
/// Adwaita Sans and about 62 of Cantarell, so both GNOME document fonts land in the range.
const COLUMN_CH: u32 = 56;

/// The find bar's own settings, matching the editor's `SearchSettings`: case-insensitive and
/// wrapping.
const FIND_OPTIONS: webkit6::FindOptions =
    webkit6::FindOptions::CASE_INSENSITIVE.union(webkit6::FindOptions::WRAP_AROUND);
/// WebKit's own guard against a query that matches the whole page; the counter says "500+" past it.
const FIND_LIMIT: u32 = 500;

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
/// The bootstrap rebuilds each fence as a `<pre class="mermaid">` from the code element's
/// `textContent`, which undoes pulldown-cmark's HTML escaping and hands mermaid the source exactly
/// as the author typed it, and puts that source back for any fence mermaid could not draw — the
/// same contract the math fallback has, so a typo never blanks a block.
const MERMAID: &str = concat!(
    include_str!("../../../vendor/mermaid/mermaid.min.js"),
    "\n",
    r#"
(function () {
  var blocks = document.querySelectorAll('pre > code.language-mermaid');
  if (!blocks.length) { return; }
  var nodes = [];
  for (var i = 0; i < blocks.length; i++) {
    var fence = blocks[i].parentElement;
    var pre = document.createElement('pre');
    pre.className = 'mermaid';
    pre.textContent = blocks[i].textContent;
    // The block's scroll-sync marker sits inside the fence and has to outlive it.
    var mark = fence.querySelector('[data-line]');
    if (mark) { fence.parentElement.insertBefore(mark, fence); }
    fence.parentElement.replaceChild(pre, fence);
    nodes.push(pre);
  }
  var sources = nodes.map(function (n) { return n.textContent; });
  var rgb = getComputedStyle(document.documentElement).backgroundColor.match(/\d+/g) || [255, 255, 255];
  var luma = (0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]) / 255;
  mermaid.initialize({ startOnLoad: false, theme: luma < 0.5 ? 'dark' : 'neutral', suppressErrorRendering: true });
  mermaid.run({ nodes: nodes }).catch(function () {}).then(function () {
    // suppressErrorRendering empties a fence it cannot parse rather than drawing an error graphic,
    // so its source goes back in and a broken diagram stays readable, as a rejected formula does.
    for (var j = 0; j < nodes.length; j++) {
      if (!nodes[j].querySelector('svg')) { nodes[j].textContent = sources[j]; }
    }
  });
})();
"#
);

struct Inner {
    view: webkit6::WebView,
    content: webkit6::UserContentManager,
    /// The sheet currently injected, so `restyle` can replace instead of stack.
    sheet: RefCell<Option<webkit6::UserStyleSheet>>,
    /// [`MERMAID`] while a note with a diagram is shown, `None` otherwise.
    mermaid: RefCell<Option<webkit6::UserScript>>,
    loaded: Cell<bool>,
    /// A line asked for while the page was still loading.
    pending: Cell<Option<u32>>,
    /// What the find bar is looking for, kept because every re-render reloads the page and
    /// WebKit's find dies with it.
    query: RefCell<Option<String>>,
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
}

impl Inner {
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
        finder.count_matches(text, FIND_OPTIONS.bits(), FIND_LIMIT);
        finder.search(text, FIND_OPTIONS.bits(), FIND_LIMIT);
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

/// One match on from `at` (1-based), wrapping at either end because [`FIND_OPTIONS`] tells WebKit
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

/// What the preview is allowed to read: a vault-relative path in, a file on *this* machine out,
/// `None` when there is none. Every window passes `Vault::fetch`, which is the file itself for a
/// local vault and a copy fetched over ssh for a remote one — so a call can block on the network.
type Resolve = dyn Fn(&str) -> Option<PathBuf> + Send + Sync;

/// Where the find readout goes; see [`Preview::connect_found`].
type Report = Box<dyn Fn(&str)>;

pub struct Preview {
    inner: Rc<Inner>,
    widget: gtk::Widget,
}

impl Preview {
    /// `resolve` turns a vault-relative asset path into a file on this machine; [`resolve_asset`]
    /// says which half of the containment guarantee is whose. `on_open` fires when the reader
    /// clicks a wikilink, with the link target as written.
    ///
    /// ponytail: every `Preview` builds its own `WebContext`, so one per tab means one WebKit
    /// process group per tab. Sharing a context (and its registered scheme) across previews is the
    /// upgrade path if tab memory ever shows up in a measurement.
    pub fn new(
        resolve: impl Fn(&str) -> Option<PathBuf> + Send + Sync + 'static,
        on_open: impl Fn(&str) + 'static,
    ) -> Preview {
        // `register_uri_scheme` asks only for `'static` and calls back on the main loop, so an `Rc`
        // would be enough to hold the resolver there — but every request hands it to a
        // `gio::spawn_blocking` worker, and crossing a thread needs `Send`. Hence `Arc`, and the
        // `Send + Sync` bound that an `Arc` of a shared closure requires.
        let resolve: Arc<Resolve> = Arc::new(resolve);

        let context = webkit6::WebContext::new();
        context.register_uri_scheme("accent", move |request| serve(&resolve, request));

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
        block_network(&content);

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
            sheet: RefCell::new(None),
            mermaid: RefCell::new(None),
            loaded: Cell::new(false),
            pending: Cell::new(None),
            query: RefCell::new(None),
            total: Cell::new(0),
            at: Cell::new(0),
            report: RefCell::new(None),
            counting: Cell::new(false),
        });

        inner.view.connect_load_changed(glib::clone!(
            #[weak]
            inner,
            move |_, event| {
                if event == webkit6::LoadEvent::Finished {
                    inner.loaded.set(true);
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
    /// [`Preview::scroll_to_line`].
    ///
    /// ponytail: `to_html` runs on the main thread, behind the editor's render debounce. Moving it
    /// to a worker is the upgrade path if a large note ever shows up in a profile.
    pub fn render(&self, rel: &str, text: &str) {
        let body = accent_core::markdown::to_html(text);
        self.inner.set_mermaid(body.contains("language-mermaid"));
        self.inner.loaded.set(false);
        self.inner
            .view
            .load_html(&document(&body), Some(&base_uri(rel)));
    }

    /// Scroll so the block containing source line `line` is visible.
    pub fn scroll_to_line(&self, line: u32) {
        if self.inner.loaded.get() {
            self.inner.scroll(line);
        } else {
            self.inner.pending.set(Some(line));
        }
    }

    /// Rebuild the stylesheet from the current GNOME theme, accent colour and document font.
    pub fn restyle(&self) {
        Self::apply_style(&self.inner);
    }

    fn apply_style(inner: &Inner) {
        let style = adw::StyleManager::default();
        // WebKit cannot resolve `var(--view-bg-color)`, so `theme` hands out the literal the
        // rest of the window resolves to under the current theme (DESIGN.md, Colour).
        let bg = theme::view_bg(style.is_dark());
        // The editor's own font, not the GNOME document font: DESIGN.md's Typography section
        // gives prose Adwaita Mono at the *size* of the document font, and the two panes are
        // meant to agree by construction. Asked of `editor::default_font`, which is where that
        // decision is made, so a change there reaches the preview without a second edit.
        let font = pango::FontDescription::from_string(&crate::editor::default_font());
        let family = font
            .family()
            .map(|f| f.to_string())
            .unwrap_or_else(|| "Adwaita Mono".to_string());
        let size = match font.size() as f64 / pango::SCALE as f64 {
            pt if pt > 0.0 => pt,
            _ => 11.0,
        };
        let css = theme_css(
            inner.view.color(),
            bg,
            style.accent_color_rgba(),
            &family,
            size,
        );

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
    // Presentation mode hides the editor column, so Ctrl+F has to address the rendered page
    // instead of the buffer. WebKit does the searching; the find bar only decides which of the
    // two it is talking to.

    /// Highlight and jump to the first match of `text`; an empty query clears the search. The
    /// query is remembered, so it survives the re-render an edit triggers.
    pub fn find(&self, text: &str) {
        *self.inner.query.borrow_mut() = Some(text.to_string());
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

/// Compile [`BLOCK_NETWORK`] and hand the filter to `content`. Compilation is asynchronous and
/// disk-backed (that is the only API WebKit offers), so the filter arrives a moment after the
/// pane does; the first note is on screen well before any of its subresources resolve.
fn block_network(content: &webkit6::UserContentManager) {
    let dir = glib::user_cache_dir()
        .join("accent")
        .join("content-filters");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("preview: no content-filter store at {}: {e}", dir.display());
        return;
    }
    let content = content.clone();
    webkit6::UserContentFilterStore::new(&dir.to_string_lossy()).save(
        "accent-block-network",
        &glib::Bytes::from_static(BLOCK_NETWORK.as_bytes()),
        gio::Cancellable::NONE,
        move |result| match result {
            Ok(filter) => content.add_filter(&filter),
            // Loud, because a preview without it can be made to talk to the network.
            Err(e) => tracing::error!("preview: network filter not installed: {e}"),
        },
    );
}

/// Route a navigation: wikilinks back to the app, web links to the browser, our own document load
/// and an anchor into it through, everything else nowhere.
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
        // The anchor rides along for a PDF, which needs the page as well as the file. Rejoined
        // here rather than kept by `accent_uri`, whose other host must go on dropping it: an
        // asset is fetched by path and a `#` in one is not a place in a document.
        Some(("open", target)) => match uri.split_once('#') {
            Some((_, anchor)) if !anchor.is_empty() => {
                on_open(&format!("{target}#{}", percent_decode(anchor)))
            }
            _ => on_open(&target),
        },
        // Our own `load_html`, which arrives as the base URI and never as a click.
        Some(("file", _)) if action.navigation_type() != webkit6::NavigationType::LinkClicked => {
            return false;
        }
        // An in-note `[text](#slug)`: WebKit scrolls to the heading `to_html` gave that `id`.
        _ if view.uri().is_some_and(|page| same_page(&uri, &page)) => return false,
        // Only a click leaves the app: a note carrying `<meta http-equiv="refresh">` or a script
        // redirect must not be able to open a browser on its own.
        _ if (uri.starts_with("http://") || uri.starts_with("https://"))
            && action.is_user_gesture() =>
        {
            let parent = view.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&uri).launch(parent.as_ref(), gio::Cancellable::NONE, |result| {
                if let Err(e) = result {
                    tracing::warn!("preview: cannot open link in browser: {e}");
                }
            });
        }
        _ => tracing::debug!("preview: refused navigation to {uri}"),
    }
    decision.ignore();
    true
}

/// Whether `uri` is a place on the page `current` shows: the same document with a `#fragment`.
/// `current` may carry a fragment of its own, from the last anchor followed.
fn same_page(uri: &str, current: &str) -> bool {
    let page = current.split_once('#').map_or(current, |(doc, _)| doc);
    uri.split_once('#').is_some_and(|(doc, _)| doc == page)
}

// ----------------------------------------------------------------------------------- uri scheme

/// Answer one `accent://file/<rel>` request, or fail it. Everything the preview is allowed to see
/// passes through here, so this is the only place a path from a note becomes a path on disk.
///
/// The answer arrives a main-loop turn later. Resolving a remote vault's asset downloads it, which
/// is an ssh round trip, and the handler runs on the main loop — a large image would freeze the
/// window. WebKit documents the way out on `register_uri_scheme`: keep a reference to the request
/// and finish it once the data is there. So the resolving goes to a `gio::spawn_blocking` worker,
/// the same pairing the git pane uses, and only the finishing comes back to the main loop.
fn serve(resolve: &Arc<Resolve>, request: &webkit6::URISchemeRequest) {
    let uri = request.uri().unwrap_or_default();
    let Some(("file", rel)) = accent_uri(&uri) else {
        return deny(request, "not a vault file");
    };
    let (resolve, request) = (resolve.clone(), request.clone());
    glib::spawn_future_local(async move {
        match gio::spawn_blocking(move || resolve_asset(&*resolve, &rel)).await {
            Ok(Some(path)) => send(&request, &path),
            Ok(None) => deny(&request, "outside the vault"),
            Err(_) => deny(&request, "the asset worker panicked"),
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
fn resolve_asset(resolve: &Resolve, rel: &str) -> Option<PathBuf> {
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
    resolve(rel)
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

/// The foreground at `alpha`, matching `highlight.rs::with_alpha`.
fn dim(c: gdk::RGBA, alpha: f32) -> String {
    css_rgba(gdk::RGBA::new(c.red(), c.green(), c.blue(), alpha))
}

/// The whole preview look, derived from three values plus the document font. The alphas are the
/// ones `highlight.rs::restyle` gives the editor, so the two panes read as one app.
fn theme_css(fg: gdk::RGBA, bg: &str, accent: gdk::RGBA, family: &str, pt: f64) -> String {
    let (text, accent) = (css_rgba(fg), css_rgba(accent));
    let (surface, quote, rule) = (dim(fg, 0.07), dim(fg, 0.6), dim(fg, 0.15));
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
         th {{ font-weight: 700; }}\n"
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

    #[test]
    fn theme_css_writes_no_hex_but_the_background() {
        for bg in [theme::view_bg(false), theme::view_bg(true)] {
            let css = theme_css(FG, bg, ACCENT, "Cantarell", 11.0);
            assert_eq!(hex_literals(&css), vec![bg], "stray hex with {bg}");
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
    fn vault_resolver(root: PathBuf) -> impl Fn(&str) -> Option<PathBuf> + Send + Sync {
        move |rel| {
            let root = root.canonicalize().ok()?;
            let path = root.join(rel).canonicalize().ok()?;
            path.starts_with(&root).then_some(path)
        }
    }

    #[test]
    fn resolve_asset_accepts_a_path_inside_the_vault() {
        let root = scratch("inside");
        std::fs::create_dir(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments/img.png"), b"x").unwrap();
        let resolve = vault_resolver(root.clone());
        assert_eq!(
            resolve_asset(&resolve, "attachments/img.png"),
            Some(root.canonicalize().unwrap().join("attachments/img.png"))
        );
        // Nothing there is the resolver's `None`, and reaches the reader as the same refusal.
        assert_eq!(resolve_asset(&resolve, "missing.png"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_asset_rejects_traversal_and_absolute_paths() {
        // A resolver that hands back whatever it is asked for, so a `None` below can only have
        // come from the check here — which is the point: it holds for any resolver.
        let naive = |rel: &str| Some(PathBuf::from(rel));
        for rel in [
            "../../../../etc/passwd",
            "notes/../../etc/passwd",
            "/etc/passwd",
            "",
        ] {
            assert_eq!(resolve_asset(&naive, rel), None, "{rel}");
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
        assert_eq!(resolve_asset(&resolve, "escape.txt"), None);
        assert_eq!(resolve_asset(&resolve, "out/secret.txt"), None);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
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
    fn base_uri_is_the_notes_own_directory() {
        assert_eq!(base_uri("Index.md"), "accent://file/");
        assert_eq!(base_uri("Notes/Deep Work.md"), "accent://file/Notes/");
        assert_eq!(base_uri("a b/c d/Note.md"), "accent://file/a%20b/c%20d/");
    }
}
