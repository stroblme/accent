//! The README's four screenshots, each taken light and dark (`make screenshots`).

use super::*;
use accent_core::config::Theme;
use accent_core::pdf::{InkStyle, PdfDoc};
use vte4::TerminalExt as _;

/// The handwriting on the PDF in scene 3, as `build-aux/screenshots/ink.txt` describes it.
const INK: &str = include_str!("../../../../build-aux/screenshots/ink.txt");

/// `ACCENT_BENCH_SHOT=<n>:<dir>`: set scene `n` up on `build-aux/screenshots.sh`'s vault, then
/// write `<dir>/shot-<n>-light.png` and `-dark.png` and print `bench shot <n> <half> <path>`.
/// Anything a scene waits for that does not come exits 1, so no broken scene is ever taken.
///
/// Light and dark are the system colour scheme, flipped in this process: the launch sets
/// `GSETTINGS_BACKEND=memory` and `ADW_DISABLE_PORTAL=1`, so libadwaita reads the scheme from a
/// GSettings that is this process's own. The capture is the X root window, popovers included.
pub(super) fn bench_shot(app: &Rc<App>, arg: &str) {
    let Some((n, dir)) = arg.split_once(':') else {
        eprintln!("ACCENT_BENCH_SHOT=<n>:<dir>, not {arg:?}");
        std::process::exit(2);
    };
    // The scenes write a PDF and type into a note.
    if !app.root().ends_with("screenshots/testvault") {
        eprintln!(
            "ACCENT_BENCH_SHOT runs on make screenshots' vault, not {:?}",
            app.root()
        );
        std::process::exit(2);
    }
    own_repository_only(app, "ACCENT_BENCH_SHOT");
    let schema = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup("org.gnome.desktop.interface", true));
    if std::env::var("GSETTINGS_BACKEND").as_deref() != Ok("memory") || schema.is_none() {
        eprintln!("ACCENT_BENCH_SHOT needs GSETTINGS_BACKEND=memory and GNOME's interface schema");
        std::process::exit(2);
    }
    let (app, n, dir) = (app.clone(), n.to_string(), dir.to_string());
    glib::spawn_future_local(async move {
        prepare(&app).await;
        let shot = Shot { n, dir };
        match shot.n.as_str() {
            "1" => scene_files(&app, &shot).await,
            "2" => scene_git(&app, &shot).await,
            "3" => scene_terminals(&app, &shot).await,
            "4" => scene_focus(&app, &shot).await,
            n => {
                eprintln!("ACCENT_BENCH_SHOT has scenes 1 to 4, not {n}");
                std::process::exit(2);
            }
        }
        bench_quit(&app);
    });
}

/// Which scene, and where its pictures go.
struct Shot {
    n: String,
    dir: String,
}

/// The window at the 1000×700 Flathub's screenshot guidelines ask for, focused, the vault indexed
/// and the text zoomed. `build-aux/screenshots.sh` runs it at scale 2.
async fn prepare(app: &Rc<App>) {
    // Without a compositor GTK draws the window's frame itself (`.solid-csd`), 5 px a side, where
    // the script adds GNOME's rounded corners and shadow. And a reader's pointer at rest has GTK's
    // overlay scrollbars hidden, where a scroll the drill makes shows them for a second.
    let look = gtk::CssProvider::new();
    look.load_from_string(
        "window.solid-csd { padding: 0; box-shadow: none; } \
         scrollbar.overlay-indicator { opacity: 0; }",
    );
    gtk::style_context_add_provider_for_display(
        &WidgetExt::display(&app.window),
        &look,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    app.window.set_default_size(1000, 700);
    // Narrower than a session's default, leaving the documents the room a 1000 px window has.
    app.restore_sidebar(true, 250);
    // No window manager gives the window the keyboard, and the pointer is parked past its corner,
    // where it raises no hover and no tooltip.
    super::git::xtest("move 400 300; focus; move 2150 1550").await;
    until("focus", 10_000, || app.window.is_active()).await;
    until("index", 120_000, || app.reconciled.get()).await;
    // The document font as it is: a 1000 px window has room for little more.
    app.set_zoom(1.0);
    println!(
        "bench shot window {}x{} {:?}",
        app.window.width(),
        app.window.height(),
        app.window.css_classes()
    );
}

/// Scene 1: the Files tree and a note with ghost text.
async fn scene_files(app: &Rc<App>, shot: &Shot) {
    let Some(tree) = app.tree.get() else {
        return println!("bench shot no_tree");
    };
    // A folder's rows arrive after it opens, so each level waits for the one above it.
    for folder in ["Code", "Submissions", "Submissions/grant-2026"] {
        until(folder, 10_000, || {
            tree::find_row(tree.model(), folder).is_some()
        })
        .await;
        if let Some(row) = tree::find_row(tree.model(), folder) {
            row.set_expanded(true);
        }
    }
    let note = "Submissions/grant-2026/Benchmark Backend.md";
    open_all(app, &[note]).await;
    let tab = tab(app, note);
    focus(app, &pane_of(app, note)).await;
    until("ghost_text", 30_000, || {
        tab.lang.support().is_some_and(|s| s.inline)
    })
    .await;
    // At the end of a paragraph under the table whose last line has room for the suggestion: one
    // is only asked for at the end of a line, and it does not wrap. The note's lines take their
    // final wrap a moment after it opens, with nothing to wait on.
    glib::timeout_future(Duration::from_millis(1000)).await;
    style::bench_painted(&tab.view, String::new).await;
    let buffer = &tab.buffer;
    let lines: Vec<String> = tab.text().lines().map(str::to_string).collect();
    let table = lines
        .iter()
        .position(|line| line.starts_with('|'))
        .unwrap_or(0);
    let roomy = |line: usize| {
        let mut end = buffer.iter_at_line(line as i32)?;
        end.forward_to_line_end();
        let mut start = end;
        tab.view.backward_display_line_start(&mut start);
        (end.offset() - start.offset() < 20).then_some(end)
    };
    let Some(end) = (table..lines.len())
        .filter(|&i| !lines[i].starts_with('|') && !lines[i].trim().is_empty())
        .find_map(roomy)
    else {
        return println!("bench shot no_paragraph");
    };
    buffer.place_cursor(&end);
    tab.view
        .scroll_to_mark(&buffer.get_insert(), 0.0, true, 0.0, 0.7);
    for dark in [false, true] {
        set_dark(app, dark).await;
        if tab.ghost_view().and_then(|v| v.ghost()).is_none() {
            crate::lang::flush(tab.clone()).await;
            crate::ghost::request(&tab).await;
        }
        until("ghost", 10_000, || {
            tab.ghost_view().and_then(|v| v.ghost()).is_some()
        })
        .await;
        app.show_chrome();
        capture(app, shot, dark).await;
    }
}

/// Scene 2: the Git pane, and the working tree against the index of a file with changes both
/// staged and not.
async fn scene_git(app: &Rc<App>, shot: &Shot) {
    // Two columns of code beside the pane, in a 1000 px window.
    app.set_zoom(0.8);
    app.show_pane("git");
    let Some(git) = app.git.get().cloned() else {
        return println!("bench shot no_git");
    };
    until("git", 60_000, || {
        git.has_repos() && git.changes_rows() > 0 && git.log_rows() >= 6
    })
    .await;
    git.compare_worktree("Code/analyze.py");
    let comparison = || app.open_tabs().iter().find_map(|tab| tab.comparison());
    until("comparison", 20_000, || comparison().is_some()).await;
    for dark in [false, true] {
        set_dark(app, dark).await;
        if let Some(compare) = comparison() {
            compare::settled(&compare).await;
        }
        app.show_chrome();
        capture(app, shot, dark).await;
    }
}

/// Scene 3: two shells beside an annotated PDF with its tool ring out, the sidebar hidden.
async fn scene_terminals(app: &Rc<App>, shot: &Shot) {
    let pdf = "Attachments/2019-03/file-17474.pdf";
    // Drawn before the PDF opens, and the file put back as it was once both halves are taken.
    let path = app.root().join(pdf);
    let original = std::fs::read(&path).map_err(anyhow::Error::from);
    let Ok(original) = original.and_then(|bytes| draw_ink(&path).map(|()| bytes)) else {
        println!("bench shot no_ink {path:?}");
        std::process::exit(1);
    };
    app.restore_sidebar(false, 250);
    open_all(app, &[pdf]).await;
    app.open_terminal();
    app.open_terminal();
    until("terminals", 10_000, || app.terminals().len() == 2).await;
    let terms = app.terminals();
    let (top, shell) = (&terms[0], &terms[1]);
    for term in [top, shell] {
        term.set_zoom(1.0);
    }

    // Pane A keeps the first shell, the second goes below it and the PDF to their right.
    let a = app.pane();
    app.split_page(&a, Side::Right, &page(app, pdf));
    app.split_page(&a, Side::Down, &shell.page);
    glib::timeout_future(Duration::from_millis(300)).await;
    divide(&a, 0.5);
    divide(&pane_of(app, pdf), 0.4);

    let text = |term: &terminal::Term| term.view.text_format(vte4::Format::Text);
    let prompts =
        |term: &terminal::Term| text(term).map_or(0, |t| t.matches("testvault $").count());
    until("prompt", 20_000, || prompts(top) > 0 && prompts(shell) > 0).await;
    top.view.feed_child(b"htop -u nobody\n");
    shell.view.feed_child(b"cat whatelse\n");
    until("htop", 10_000, || {
        text(top).is_some_and(|t| t.contains("Tasks:"))
    })
    .await;
    until("cat", 10_000, || prompts(shell) > 1).await;

    focus(app, &pane_of(app, pdf)).await;
    app.set_drawing(true);
    let pdf_tab = app.active_pdf();
    until("ring", 5_000, || {
        pdf_tab.as_ref().is_some_and(|p| p.ring_visible())
    })
    .await;
    // The shell is what the reader was last typing in, as in the original.
    if let Some(pane) = app.pane_of(&shell.page) {
        focus(app, &pane).await;
    }
    for dark in [false, true] {
        set_dark(app, dark).await;
        until("tiles", 10_000, || {
            pdf_tab
                .as_ref()
                .is_some_and(|p| matches!(p.tiles(), (0, 0, _)))
        })
        .await;
        app.show_chrome();
        capture(app, shot, dark).await;
    }
    if let Err(e) = std::fs::write(&path, original) {
        println!("bench shot pdf_not_restored {e}");
    }
    // The holder leaves with its last shell, so nothing of the scene outlives its launch.
    for term in &terms {
        term.kill();
    }
    until("shells", 10_000, || app.terminals().is_empty()).await;
}

/// Scene 4: Solarized, Focus Mode High with the chrome faded, and the link popup over a note.
async fn scene_focus(app: &Rc<App>, shot: &Shot) {
    // The faded sidebar would keep its column, and the note sit off the window's middle.
    app.restore_sidebar(false, 250);
    crate::theme::apply(Theme::Solarized);
    app.restyle_all();
    app.config.borrow_mut().focus_mode = FocusMode::High;
    let rel = "Daily/Observable Paragraph Information Cluster Archive.md";
    let Some((tab, session)) = complete::bench_complete_open(app, rel).await else {
        std::process::exit(1);
    };
    let own = tab.text();
    // The first link on line 38, its target typed again up to the popup: what a reader linking
    // another note sees.
    let buffer = &tab.buffer;
    let Some(mut start) = buffer.iter_at_line(37) else {
        return println!("bench shot no_line");
    };
    let mut end = start;
    end.forward_to_line_end();
    let line = buffer.text(&start, &end, false);
    let Some((at, len)) = line
        .find("[[")
        .and_then(|at| Some((at + 2, line[at + 2..].find("]]")?)))
    else {
        return println!("bench shot no_link");
    };
    start.forward_chars(line[..at].chars().count() as i32);
    end = start;
    end.forward_chars(line[at..at + len].chars().count() as i32);
    buffer.delete(&mut start, &mut end);
    buffer.place_cursor(&start);
    // Once the note is laid out in its theme, or the scroll lands on estimated line heights.
    style::bench_painted(&tab.view, String::new).await;
    tab.view
        .scroll_to_mark(&buffer.get_insert(), 0.0, true, 0.0, 0.55);
    style::bench_painted(&tab.view, String::new).await;
    for c in "Cryos".chars() {
        complete::bench_char(&tab, c);
        glib::timeout_future(Duration::from_millis(1)).await;
    }
    complete::bench_settle(&session).await;
    until("popup", 10_000, || session.is_shown()).await;
    for dark in [false, true] {
        set_dark(app, dark).await;
        until("popup", 10_000, || session.is_shown()).await;
        app.hide_chrome();
        capture(app, shot, dark).await;
    }
    tab.set_text(&own);
    if let Err(e) = app.write_tab(&tab, None) {
        println!("bench shot note_not_restored {e}");
    }
}

/// Write the strokes [`INK`] lists into the PDF at `path`.
fn draw_ink(path: &Path) -> anyhow::Result<()> {
    let mut doc = PdfDoc::open(path)?;
    for line in INK.lines().filter(|line| !line.starts_with('#')) {
        let mut words = line.split(' ');
        let mut number = || -> anyhow::Result<f32> {
            Ok(words
                .next()
                .ok_or_else(|| anyhow::anyhow!("short line"))?
                .parse()?)
        };
        let rgba = [number()?, number()?, number()?, number()?].map(|v| v as u8);
        let (width, multiply) = (number()?, number()? == 1.0);
        let points = words
            .map(|xy| {
                let (x, y) = xy
                    .split_once(',')
                    .ok_or_else(|| anyhow::anyhow!("not x,y"))?;
                Ok((x.parse()?, y.parse()?))
            })
            .collect::<anyhow::Result<Vec<(f32, f32)>>>()?;
        let style = InkStyle {
            width,
            rgba,
            multiply,
        };
        doc.add_ink(0, &points, style)?;
    }
    let bytes = doc.save()?;
    drop(doc);
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Flip the system colour scheme and wait for the window's restyle to have run.
async fn set_dark(app: &Rc<App>, dark: bool) {
    let scheme = if dark { "prefer-dark" } else { "default" };
    let interface = gio::Settings::new("org.gnome.desktop.interface");
    if let Err(e) = interface.set_string("color-scheme", scheme) {
        println!("bench shot no_scheme {e}");
        std::process::exit(1);
    }
    until("scheme", 5_000, || {
        adw::StyleManager::default().is_dark() == dark
    })
    .await;
    // `restyle_all` waits for the next layout, and what it restyles repaints after that.
    style::bench_painted(&app.window, String::new).await;
    glib::timeout_future(Duration::from_millis(300)).await;
}

/// Take the screen once the window has painted, at the window's own size in pixels.
async fn capture(app: &Rc<App>, shot: &Shot, dark: bool) {
    // One that came after `prepare`, such as the first index's: said here, kept off the picture.
    let toasts = app.toasts.shown();
    if !toasts.is_empty() {
        println!("bench shot toasts {toasts:?}");
        app.toasts.dismiss_all();
    }
    // GTK's cairo renderer repaints only what changed since the last frame, and a line repainted
    // on its own cuts the glyphs of its neighbours where line heights are fractional. A frame at
    // another opacity changes every pixel, so the one after it is painted whole.
    app.window.set_opacity(0.99);
    style::bench_painted(&app.window, String::new).await;
    app.window.set_opacity(1.0);
    style::bench_painted(&app.window, String::new).await;
    glib::timeout_future(Duration::from_millis(150)).await;
    let half = if dark { "dark" } else { "light" };
    let path = format!("{}/shot-{}-{half}.png", shot.dir, shot.n);
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let scale = app.window.scale_factor();
    let size = format!(
        "{}x{}+0+0",
        app.window.width() * scale,
        app.window.height() * scale
    );
    let argv = [
        "magick", "import", "-display", &display, "-window", "root", "-crop", &size, "+repage",
        &path,
    ];
    let run = gio::Subprocess::newv(&argv.map(std::ffi::OsStr::new), gio::SubprocessFlags::NONE);
    if let Err(e) = match run {
        Ok(run) => run.wait_check_future().await,
        Err(e) => Err(e),
    } {
        println!("bench shot import_failed {e}");
        std::process::exit(1);
    }
    println!("bench shot {} {half} {path}", shot.n);
}

/// Poll `ready` every 50 ms; past `ms`, say what never came and exit 1.
async fn until(what: &str, ms: u64, ready: impl Fn() -> bool) {
    let t = Instant::now();
    while !ready() {
        if t.elapsed() > Duration::from_millis(ms) {
            println!("bench shot timeout {what}");
            std::process::exit(1);
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

/// Open every one of `rels` and wait until each has a tab.
async fn open_all(app: &Rc<App>, rels: &[&str]) {
    for rel in rels {
        app.open_path(rel);
    }
    until("open", 20_000, || {
        rels.iter().all(|rel| app.doc_for(rel).is_some())
    })
    .await;
}

/// Give `pane`'s document the keyboard, as a click into it does, and with it the active pane.
async fn focus(app: &Rc<App>, pane: &Rc<Pane>) {
    app.focus_document(pane);
    until("focus", 5_000, || Rc::ptr_eq(&app.pane(), pane)).await;
}

fn page(app: &Rc<App>, rel: &str) -> adw::TabPage {
    app.doc_for(rel).expect("an open tab").page().clone()
}

fn pane_of(app: &Rc<App>, rel: &str) -> Rc<Pane> {
    app.pane_of(&page(app, rel)).expect("a pane")
}

fn tab(app: &Rc<App>, rel: &str) -> Rc<Tab> {
    app.doc_for(rel)
        .and_then(|doc| doc.tab().cloned())
        .expect("a text tab")
}

/// Put the divider of the split right around `pane` at `share` of the split's extent.
fn divide(pane: &Pane, share: f64) {
    let Some(paned) = pane.widget().parent().and_downcast::<gtk::Paned>() else {
        return;
    };
    let extent = match paned.orientation() {
        gtk::Orientation::Vertical => paned.height(),
        _ => paned.width(),
    };
    paned.set_position((f64::from(extent) * share).round() as i32);
}
