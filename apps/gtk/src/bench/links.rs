//! The terminal's file links: what lights under the pointer and what a Ctrl+click opens, through
//! XTEST as a reader's pointer reaches them.

use super::files::WAIT;
use super::git::xtest;
use super::*;
use vte4::TerminalExt as _;

/// `ACCENT_BENCH_LINKS=1` on a local vault, under a scratch HOME: a shell at the vault root is
/// fed a screen of paths as Claude Code, a compiler and grep print them, a missing one, a URL, an
/// OSC 8 `file:` link and an OSC 8 link to a bare path, as Claude Code may write a markdown link.
/// For each the pointer rests on it and the drill prints whether it lit, then a Ctrl+click and
/// the tab and caret it left in front, the shell brought back after each; then whether a path
/// lights on the one motion that reaches it, and the context menu over a path. Writes `links/target.rs` in the vault and `outside.md` in HOME.
pub(super) fn bench_links(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let root = app.root();
        let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let target = root.join("links/target.rs");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, lines).unwrap();
        let home = glib::home_dir();
        std::fs::write(home.join("outside.md"), "a\nb\nc\n").unwrap();
        app.open_terminal();
        let Some(term) = until("links", "shell", WAIT, || app.terminals().first().cloned()).await
        else {
            return bench_quit(&app);
        };
        until("links", "prompt", WAIT, || {
            !super::debug::text(&term).trim().is_empty()
        })
        .await;
        let uri = glib::filename_to_uri(&target, None).unwrap();
        let screen = [
            "Read(links/target.rs)".to_string(),
            "see links/target.rs:12:3.".to_string(),
            "~/outside.md:2".to_string(),
            "missing/nothing.rs:4".to_string(),
            "https://example.org/links/target.rs".to_string(),
            format!("\x1b]8;;{uri}:5\x1b\\osc8 link\x1b]8;;\x1b\\"),
            "\x1b]8;;links/target.rs#L7\x1b\\markdown link\x1b]8;;\x1b\\".to_string(),
        ];
        // A screen of its own, the prompt cleared away.
        term.view.feed(b"\x1b[2J\x1b[H");
        for line in &screen {
            term.view.feed(format!("{line}\r\n").as_bytes());
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let steps = [
            ("read", 0, "links"),
            ("line_col", 1, "links"),
            ("home", 2, "outside"),
            ("missing", 3, "missing"),
            ("url", 4, "example"),
            ("osc8", 5, "osc8"),
            ("markdown", 6, "markdown"),
        ];
        for (name, row, word) in steps {
            let shown = super::debug::text(&term);
            let col = shown
                .lines()
                .nth(row)
                .and_then(|l| l.find(word))
                .unwrap_or(0);
            let (x, y) = cell(&term, row, col + 1);
            // A step on within the cell, as a pointer comes to rest: a file found off the main
            // thread lights on the motion after its answer.
            let to = screen_at(&app, &term, x, y);
            let on = screen_at(&app, &term, x + 1.0, y);
            xtest(&format!("move {to}; sleep 0.3; move {on}")).await;
            let lit = term.lit_at(x, y);
            let url = term.view.check_match_at(x, y).0.filter(|_| lit.is_none());
            let osc8 = term.view.check_hyperlink_at(x, y);
            // The pointer VTE shows there, which it gives a match it lights.
            let cursor = term.view.cursor().and_then(|c| c.name());
            println!(
                "bench links hover {name} lit={lit:?} url={url:?} osc8={} cursor={cursor:?}",
                osc8.is_some()
            );
            if name == "url" {
                continue;
            }
            xtest("keydown ctrl; down; up; keyup ctrl").await;
            println!("bench links open {name} {}", in_front(&app, &home));
            app.reveal_page(&term.page);
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        // A path found at once lights on the one motion that reaches it, ahead of VTE's own look.
        let (x, y) = cell(&term, 3, 1);
        xtest(&format!("move {}", screen_at(&app, &term, x, y))).await;
        let (x, y) = cell(&term, 2, 3);
        xtest(&format!("move {}", screen_at(&app, &term, x, y))).await;
        let cursor = term.view.cursor().and_then(|c| c.name());
        println!("bench links first cursor={cursor:?}");
        // The context menu over a path offers it as it offers a URL, the address its absolute path.
        let (x, y) = cell(&term, 1, 6);
        xtest(&format!(
            "move {}; down 3; up 3",
            screen_at(&app, &term, x, y)
        ))
        .await;
        let menu = term.view.context_menu_model();
        let labels = menu.as_ref().map(crate::fileops::labels);
        let links = menu.and_then(|m| m.item_link(m.n_items() - 1, gio::MENU_LINK_SECTION));
        let copied = links
            .and_then(|l| l.item_attribute_value(1, gio::MENU_ATTRIBUTE_TARGET, None))
            .and_then(|v| {
                v.str()
                    .map(|path| path.replace(&*root.to_string_lossy(), "<vault>"))
            });
        println!("bench links menu {labels:?} copies={copied:?}");
        xtest("key Escape").await;
        let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
        glib::timeout_future(Duration::from_millis(300)).await;
        bench_quit(&app);
    });
}

/// The middle of the cell at `row`, `col` of `term`, in its own coordinates.
fn cell(term: &terminal::Term, row: usize, col: usize) -> (f64, f64) {
    let (w, h) = (
        term.view.char_width() as f64,
        term.view.char_height() as f64,
    );
    ((col as f64 + 0.5) * w, (row as f64 + 0.5) * h)
}

/// `(x, y)` of `term` on the screen, as `build-aux/xtest.py` takes a point.
fn screen_at(app: &App, term: &terminal::Term, x: f64, y: f64) -> String {
    let p = graphene::Point::new(x as f32, y as f32);
    let p = term.view.compute_point(&app.window, &p).unwrap_or(p);
    let (sx, sy) = app.window.surface_transform();
    format!("{} {}", f64::from(p.x()) + sx, f64::from(p.y()) + sy)
}

/// The tab in front and its caret's line and column, HOME written as `~`.
fn in_front(app: &App, home: &std::path::Path) -> String {
    match app.active_doc() {
        Some(Doc::Text(tab)) => {
            let at = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
            let key = tab.rel().replace(&*home.to_string_lossy(), "~");
            format!("{key} ({}, {})", at.line(), at.line_offset())
        }
        Some(Doc::Terminal(_)) => "terminal".to_string(),
        Some(_) => "other".to_string(),
        None => "none".to_string(),
    }
}
