//! The README's seven screenshots, each taken light and dark (`make screenshots`).

use super::*;
use accent_core::config::{Editor, Theme};
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
            "3" => scene_editor(&app, &shot).await,
            "4" => scene_focus(&app, &shot).await,
            "5" => scene_debug(&app, &shot).await,
            "6" => scene_diagram(&app, &shot).await,
            "7" => scene_notebook(&app, &shot).await,
            n => {
                eprintln!("ACCENT_BENCH_SHOT has scenes 1 to 7, not {n}");
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
    // A folder's rows arrive after it opens, so each level waits for the one above it. Code stays
    // shut, its tint saying it holds a change: open, its rows would push the note's off the list.
    for folder in ["Submissions", "Submissions/grant-2026"] {
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
    // The open note's row lit and on screen, as Reveal in Sidebar leaves it: the tree's selection
    // follows the tab but never scrolls to it.
    until("row", 10_000, || {
        tree::find_row(tree.model(), note).is_some()
    })
    .await;
    tree.reveal(note);
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
            crate::ghost::request(tab.as_ref()).await;
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
    // The pane is there once its first refresh has found the repository.
    until("git_pane", 60_000, || {
        app.sidebar.get().is_some_and(|s| s.has_pane("git"))
    })
    .await;
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

/// Scene 3's script, opened in Helix at the function its unstaged change adds.
const SCRIPT: &str = "Code/analyze.py";
const SCRIPT_LINE: u32 = 37;

/// Scene 3: Helix, picked as the text editor, on a script beside an annotated PDF with its tool
/// ring out, the sidebar hidden.
async fn scene_editor(app: &Rc<App>, shot: &Shot) {
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
    // As Preferences picks it, but kept out of the config file the next scenes read.
    app.config.borrow_mut().editor = Editor::Helix;
    app.edit_at(SCRIPT, (SCRIPT_LINE, None), Opened::Kept);
    let text = |term: &terminal::Term| {
        let text = term.view.text_format(vte4::Format::Text);
        text.map(|t| t.to_string()).unwrap_or_default()
    };
    // Helix's status line once it has drawn the file.
    until("helix", 20_000, || {
        app.editor_of(SCRIPT)
            .is_some_and(|term| text(&term).contains("NOR"))
    })
    .await;
    let Some(helix) = app.editor_of(SCRIPT) else {
        return println!("bench shot no_helix");
    };
    // More of each line, as a code tab beside another document has.
    helix.set_zoom(0.8);

    // Helix keeps the first pane, the PDF goes to its right.
    let a = app.pane();
    app.split_page(&a, Side::Right, &page(app, pdf));
    glib::timeout_future(Duration::from_millis(300)).await;
    divide(&pane_of(app, pdf), 0.5);

    focus(app, &pane_of(app, pdf)).await;
    app.set_drawing(true);
    let pdf_tab = app.active_pdf();
    until("ring", 5_000, || {
        pdf_tab.as_ref().is_some_and(|p| p.ring_visible())
    })
    .await;
    // Dragged from its top corner to the bottom one, off the handwriting of a page this narrow.
    style::bench_painted(&app.window, String::new).await;
    let ring = pdf_tab.as_ref().map(|p| p.ring().widget().clone());
    if let Some(ring) = ring
        && let Some(overlay) = ring.parent()
        && let Some(corner) = ring.compute_point(&overlay, &graphene::Point::zero())
    {
        let bottom = overlay.height() - ring.height() - corner.y() as i32;
        app.ring_at
            .set(Some((f64::from(corner.x()), f64::from(bottom))));
        app.set_drawing(true);
    }
    // Helix is what the reader was last typing in.
    if let Some(pane) = app.pane_of(&helix.page) {
        focus(app, &pane).await;
    }
    for dark in [false, true] {
        if dark {
            // Helix's themes in the terminal's colours, which it does not swap by itself.
            helix.view.feed_child(b":theme term16_dark\r");
        }
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
    helix.kill();
    until("helix_ended", 10_000, || {
        app.terminals().iter().all(|term| term.ended())
    })
    .await;
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

/// Scene 5's program, `build-aux/screenshots/readout.py`, and its breakpoints by 1-based line:
/// `fidelity`'s return, a weak qubit noted, and the summary.
const PROGRAM: &str = "Code/readout.py";
const BREAKPOINTS: [u32; 3] = [17, 27, 28];
/// What the Debug Console is asked at the stop.
const ASKED: [&str; 3] = ["p0, p1", "(p0 + p1) / 2", "one[0] / sum(one)"];

/// Scene 5: Solarized, a Python program paused in its third call to `fidelity`, the Code pane
/// open on the calls and the locals, the Debug Console over the program's own terminal to its
/// right.
async fn scene_debug(app: &Rc<App>, shot: &Shot) {
    crate::theme::apply(Theme::Solarized);
    app.restyle_all();
    // Code beside two terminals, in a 1000 px window.
    app.set_zoom(0.8);
    open_all(app, &[PROGRAM]).await;
    let tab = tab(app, PROGRAM);
    for line in BREAKPOINTS {
        app.toggle_breakpoint(&tab, line - 1);
    }
    // Opened as a click on its switcher does: Start Debugging leaves the sidebar as it is.
    until("code", 5_000, || {
        app.sidebar.get().is_some_and(|s| s.has_pane("code"))
    })
    .await;
    app.show_pane("code");
    // Variables given the room of all four locals, as a reader drags the divider above
    // Breakpoints: the session keeps it so.
    if let Some(sidebar) = app.sidebar.get() {
        let mut saved = sidebar.code_saved();
        saved.dividers = vec![None, None, None, Some(0.6)];
        sidebar.restore_code(&saved);
    }
    let _ = WidgetExt::activate_action(&app.window, "win.debug-start", None);
    // On q2, past the stops on q0 and q1, whose lines the program has printed by then.
    loop {
        until("stopped", 30_000, || {
            super::debug::paused_line(&tab).is_some()
        })
        .await;
        let locals = || super::debug::shown(app).2;
        until("locals", 10_000, || !locals().is_empty()).await;
        let on_q2 = locals().iter().any(|value| value == "zero=(978, 22)");
        if super::debug::paused_line(&tab) == Some(BREAKPOINTS[0] - 1) && on_q2 {
            break;
        }
        let _ = WidgetExt::activate_action(&app.window, "win.debug-continue", None);
    }
    let (Some(console), Some(program)) = (app.console(), super::debug::program_tab(app)) else {
        println!("bench shot no_debug_terminals");
        std::process::exit(1);
    };

    // The code keeps its pane, the console goes to its right and the program's tab below that.
    let code = pane_of(app, PROGRAM);
    app.split_page(&code, Side::Right, &console.page);
    let Some(right) = app.pane_of(&console.page) else {
        return println!("bench shot no_console_pane");
    };
    app.split_page(&right, Side::Down, &program.page);
    glib::timeout_future(Duration::from_millis(300)).await;
    divide(&code, 0.55);
    divide(&right, 0.5);
    // Only now that its tab is on screen: VTE keeps what a terminal behind another tab is sent and
    // puts it on that terminal's screen once it is shown, so the lines printed meanwhile
    // are not in its text before.
    until("output", 10_000, || {
        super::debug::text(&program).contains("q1  0.9270")
    })
    .await;

    // Typed at the console in front with the keyboard, as a reader brings it forward.
    let _ = WidgetExt::activate_action(&app.window, "win.debug-console", None);
    for asked in ASKED {
        app.console_typed(&format!("{asked}\r"));
        until("repl", 10_000, || {
            super::debug::answer(&super::debug::text(&console), asked).is_some()
        })
        .await;
    }
    // From `LIMIT` down: the function paused in, the loop calling it and every breakpoint.
    if let Some(top) = tab.buffer.iter_at_line(9) {
        let mark = tab.buffer.create_mark(None, &top, true);
        tab.view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.0);
        tab.buffer.delete_mark(&mark);
    }
    for dark in [false, true] {
        set_dark(app, dark).await;
        app.show_chrome();
        capture(app, shot, dark).await;
    }
    let _ = WidgetExt::activate_action(&app.window, "win.debug-stop", None);
    until("ended", 30_000, || !app.debugger.running()).await;
}

/// Scene 6's diagram, `build-aux/screenshots/readout-chain.drawio.xml`.
const DIAGRAM: &str = "Submissions/grant-2026/figures/readout-chain.drawio";

/// Scene 6: a draw.io diagram of a readout chain, its pages in the Outline over their
/// thumbnails, the drawing tools put away.
async fn scene_diagram(app: &Rc<App>, shot: &Shot) {
    open_all(app, &[DIAGRAM]).await;
    until("diagram", 10_000, || {
        app.active_diagram().is_some_and(|tab| tab.laid_out())
    })
    .await;
    // Out by default, the ring would cover the page's top corner.
    app.set_drawing(false);
    app.show_pane("outline");
    for dark in [false, true] {
        set_dark(app, dark).await;
        app.show_chrome();
        capture(app, shot, dark).await;
    }
}

/// Scene 7's notebook, `build-aux/screenshots/calibration.ipynb`, its outputs stored as a run
/// left them.
const NOTEBOOK: &str = "Notebooks/calibration.ipynb";

/// Scene 7: a notebook on its sweep's code and plot, reached through the sweep's heading in the
/// Outline.
async fn scene_notebook(app: &Rc<App>, shot: &Shot) {
    // A cell of code and its plot together, in a 1000 px window.
    app.set_zoom(0.8);
    open_all(app, &[NOTEBOOK]).await;
    let (Some(tab), Some(sidebar)) = (app.active_notebook(), app.sidebar.get()) else {
        return println!("bench shot no_notebook");
    };
    app.show_pane("outline");
    until("outline", 10_000, || sidebar.outline_lines(None).len() == 4).await;
    // As a click on the heading does, and a scroll on until its cell is at the top, so the plot
    // under it shows whole.
    sidebar.outline_lines(Some(2));
    let page = tab.bench_range().1;
    let shown = || tab.bench_cell_y(4).filter(|y| (0.0..page).contains(y));
    until("jump", 5_000, || shown().is_some()).await;
    if let Some(y) = shown() {
        tab.bench_scroll_by((y - 12.0) / page);
    }
    for dark in [false, true] {
        set_dark(app, dark).await;
        // The plot is decoded on a worker, and again in the dark half's colours.
        until("plot", 10_000, || {
            let c = tab.counts().3;
            let drawn = tab.bench_pictures().iter().any(|&(w, h, _)| w > 0 && h > 0);
            drawn && c.pictures > 0 && (c.recoloured == c.pictures) == dark
        })
        .await;
        app.show_chrome();
        capture(app, shot, dark).await;
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
        doc.add_ink(0, &points, style, 1.0)?;
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
    // Nothing on the bar's busy line, "Indexing…" above all. The reading of the PDFs' text would
    // go on for some 15 minutes over the vault's 12 975 PDFs, so it is stopped as its Stop does.
    until("status", 60_000, || {
        let busy = app.statusbar.progress_text();
        if busy.starts_with("Indexing PDF text") {
            app.statusbar.pdf_text_control().emit_clicked();
        }
        busy.is_empty()
    })
    .await;
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

/// Wait for `ready` up to `ms` ([`super::until`]); past that, exit 1.
async fn until(what: &str, ms: u64, ready: impl Fn() -> bool) {
    if super::until("shot", what, ms, ready).await.is_none() {
        std::process::exit(1);
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
