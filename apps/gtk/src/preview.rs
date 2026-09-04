//! Read-only markdown preview: a WebKitGTK view fed by `markdown::to_html`.
//!
//! The pane renders notes that may have been synced from another machine, so it is built to be a
//! dead end: an ephemeral network session, no page JavaScript, a content filter that refuses every
//! load, and a custom `accent:` scheme that only ever hands out files from inside the vault.
//!
//! WebKit cannot read GTK's CSS variables, so the stylesheet is generated in Rust from the same
//! three sources `highlight.rs` uses — foreground, background, accent — and injected as a user
//! stylesheet. Editor and preview therefore agree by construction, in every theme and accent.

use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use webkit6::prelude::*;

/// libadwaita's `--view-bg-color`. DESIGN.md: the one hex pair the codebase is allowed, because
/// WebKit cannot resolve `var(--view-bg-color)` itself.
const LIGHT_BG: &str = "#ffffff";
const DARK_BG: &str = "#1d1d20";

/// Prose column width, in `ch`. DESIGN.md asks for 60 to 72 characters, and `ch` is the advance
/// of "0", which is wider than the average letter: measured on this pane, 56ch is 71 characters of
/// Adwaita Sans and about 62 of Cantarell, so both GNOME document fonts land in the range.
const COLUMN_CH: u32 = 56;

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
  if (found) { (found.parentElement || found).scrollIntoView({ block: 'start' }); }
};
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
}

impl Inner {
    fn scroll(&self, line: u32) {
        self.view.evaluate_javascript(
            &format!("window.__accentScrollToLine({line})"),
            None,
            None,
            gio::Cancellable::NONE,
            |_| (),
        );
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

pub struct Preview {
    inner: Rc<Inner>,
    widget: gtk::Widget,
}

impl Preview {
    /// `root` is the canonical vault directory; the custom URI scheme serves files from inside it.
    /// `on_open` fires when the reader clicks a wikilink, with the link target as written.
    ///
    /// ponytail: every `Preview` builds its own `WebContext`, so one per tab means one WebKit
    /// process group per tab. Sharing a context (and its registered scheme) across previews is the
    /// upgrade path if tab memory ever shows up in a measurement.
    pub fn new(root: PathBuf, on_open: impl Fn(&str) + 'static) -> Preview {
        let root = root.canonicalize().unwrap_or(root);

        let context = webkit6::WebContext::new();
        context.register_uri_scheme("accent", move |request| serve(&root, request));

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
        let bg = if style.is_dark() { DARK_BG } else { LIGHT_BG };
        let font = pango::FontDescription::from_string(&style.document_font_name());
        let family = font
            .family()
            .map(|f| f.to_string())
            .unwrap_or_else(|| "Cantarell".to_string());
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
/// through, everything else nowhere.
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
        Some(("open", target)) => on_open(&target),
        // Our own `load_html`, which arrives as the base URI and never as a click.
        Some(("file", _)) if action.navigation_type() != webkit6::NavigationType::LinkClicked => {
            return false;
        }
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

// ----------------------------------------------------------------------------------- uri scheme

/// Answer one `accent://file/<rel>` request, or fail it. Everything the preview is allowed to see
/// passes through here, so this is the only place a path from a note becomes a path on disk.
fn serve(root: &Path, request: &webkit6::URISchemeRequest) {
    let deny = |what: &str| {
        let mut error = glib::Error::new(gio::IOErrorEnum::PermissionDenied, what);
        request.finish_error(&mut error);
    };
    let uri = request.uri().unwrap_or_default();
    let Some(("file", rel)) = accent_uri(&uri) else {
        return deny("not a vault file");
    };
    let Some(path) = resolve_asset(root, &rel) else {
        return deny("outside the vault");
    };
    let file = gio::File::for_path(&path);
    match file.read(gio::Cancellable::NONE) {
        Ok(stream) => {
            let size = std::fs::metadata(&path).map_or(-1, |m| m.len() as i64);
            let mime = gio::content_type_guess(Some(&path), None).0;
            request.finish(&stream, size, Some(&mime));
        }
        Err(e) => deny(&e.to_string()),
    }
}

/// `root`-relative asset path -> a real file inside the vault, or `None`.
///
/// Canonicalising both sides and re-checking the prefix is what stops `![[../../../etc/passwd]]`
/// and a symlink that points out of the vault; neither survives the comparison.
fn resolve_asset(root: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let path = root.join(rel).canonicalize().ok()?;
    path.starts_with(&root).then_some(path)
}

/// Split `accent://<host>/<path>` into host and percent-decoded path, dropping `?query` and
/// `#fragment`. `None` for anything that is not an `accent:` URI.
fn accent_uri(uri: &str) -> Option<(&str, String)> {
    let rest = uri.strip_prefix("accent://")?;
    let head = rest.split_once(['?', '#']).map_or(rest, |(h, _)| h);
    let (host, path) = head.split_once('/').unwrap_or((head, ""));
    Some((host, percent_decode(path)))
}

/// Percent-decode a URI component. A malformed escape is left as written.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let pair = (i + 2 < bytes.len())
            .then(|| Some(hex(bytes[i + 1])? * 16 + hex(bytes[i + 2])?))
            .flatten();
        match pair {
            Some(byte) if bytes[i] == b'%' => {
                out.push(byte);
                i += 3;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

// -------------------------------------------------------------------------------------- document

/// Where relative links in `rel`'s markdown resolve from: the note's own directory.
fn base_uri(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => format!(
            "accent://file/{}/",
            glib::Uri::escape_string(dir, Some("/"), false)
        ),
        _ => "accent://file/".to_string(),
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
        let css = theme_css(FG, DARK_BG, ACCENT, "Cantarell", 11.0);
        assert!(
            css.contains("a { color: rgba(255, 0, 0, 1.00); text-decoration: underline; }"),
            "{css}"
        );
        assert!(css.contains("border-left: 3px solid rgba(255, 0, 0, 1.00)"));
    }

    #[test]
    fn theme_css_writes_no_hex_but_the_background() {
        for bg in [LIGHT_BG, DARK_BG] {
            let css = theme_css(FG, bg, ACCENT, "Cantarell", 11.0);
            assert_eq!(hex_literals(&css), vec![bg], "stray hex with {bg}");
        }
    }

    #[test]
    fn theme_css_follows_its_inputs() {
        let base = theme_css(FG, LIGHT_BG, ACCENT, "Cantarell", 11.0);
        let teal = gdk::RGBA::new(0.0, 0.5, 0.5, 1.0);
        assert_ne!(base, theme_css(FG, DARK_BG, ACCENT, "Cantarell", 11.0));
        assert_ne!(base, theme_css(FG, LIGHT_BG, teal, "Cantarell", 11.0));
        assert_ne!(base, theme_css(FG, LIGHT_BG, ACCENT, "Inter", 11.0));
        assert_ne!(base, theme_css(FG, LIGHT_BG, ACCENT, "Cantarell", 13.0));
        assert!(theme_css(FG, LIGHT_BG, ACCENT, "Inter", 13.0).contains("\"Inter\""));
    }

    #[test]
    fn resolve_asset_accepts_a_path_inside_the_vault() {
        let root = scratch("inside");
        std::fs::create_dir(root.join("attachments")).unwrap();
        std::fs::write(root.join("attachments/img.png"), b"x").unwrap();
        assert_eq!(
            resolve_asset(&root, "attachments/img.png"),
            Some(root.canonicalize().unwrap().join("attachments/img.png"))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_asset_rejects_traversal_and_absolute_paths() {
        let root = scratch("traversal");
        std::fs::create_dir(root.join("notes")).unwrap();
        assert_eq!(resolve_asset(&root, "../../../../etc/passwd"), None);
        assert_eq!(resolve_asset(&root, "notes/../../etc/passwd"), None);
        assert_eq!(resolve_asset(&root, "/etc/passwd"), None);
        assert_eq!(resolve_asset(&root, ""), None);
        assert_eq!(resolve_asset(&root, "missing.png"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_asset_rejects_a_symlink_out_of_the_vault() {
        let root = scratch("symlink");
        let outside = scratch("symlink-target");
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();

        assert_eq!(resolve_asset(&root, "escape.txt"), None);
        assert_eq!(resolve_asset(&root, "out/secret.txt"), None);
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
    fn base_uri_is_the_notes_own_directory() {
        assert_eq!(base_uri("Index.md"), "accent://file/");
        assert_eq!(base_uri("Notes/Deep Work.md"), "accent://file/Notes/");
        assert_eq!(base_uri("a b/c d/Note.md"), "accent://file/a%20b/c%20d/");
    }
}
