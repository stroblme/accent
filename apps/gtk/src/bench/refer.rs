//! Files rows dropped with Shift into what a pane shows (`ACCENT_BENCH_DROP=refer:`).

use super::files::until;
use super::*;
use vte4::TerminalExt as _;

/// The drill's own files, written under `refer/` in the vault.
const NOTE: &str = "refer/note.md";
const CODE: &str = "refer/src/code.py";
const ROW: &str = "refer/target.md";
const IMAGE: &str = "refer/fig.png";
const FOLDER: &str = "refer/sub";
const PDF: &str = "refer/paper.pdf";
const NOTEBOOK: &str = "refer/book.ipynb";

/// `ACCENT_BENCH_DROP=refer:1`: real XTEST drags of Files rows into the middle of the pane,
/// Shift going down on the way or not at all, onto a note, a code file, a notebook's cells, a
/// shell and a PDF in turn: with Shift the note gets a link (a marked set a link a line, as one
/// undo step; a folder nothing), the code file the row's path from its folder, a markdown cell a
/// link and a code cell a path, the shell the quoted absolute path typed at its prompt, and the
/// PDF what a plain drop gives it, the row opened there. Writes its files into the vault; exits
/// 1 on a wrong answer.
pub(super) fn bench_refer(app: &Rc<App>, _arg: &str) {
    let root = app.root();
    for (rel, text) in [
        (NOTE, "# Note\n\nSome text here.\n"),
        (CODE, "import os\n\nprint('hi')\n"),
        (ROW, "# Target\n"),
        (IMAGE, "not really a picture"),
        ("refer/sub/inner.md", "# Inner\n"),
        (PDF, "%PDF-1.4\n"),
        (
            NOTEBOOK,
            // A tall first cell, so the two after it sit in the middle of the pane, clear of
            // the edge that splits it.
            r##"{"cells": [{"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [],
  "source": ["a = 1\n", "b = 2\n", "c = 3\n", "d = 4\n", "e = 5\n", "f = 6\n", "g = 7\n", "h = 8"]},
 {"cell_type": "markdown", "metadata": {}, "source": ["# Book"]},
 {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [], "source": ["x = 1"]}],
 "metadata": {}, "nbformat": 4, "nbformat_minor": 5}"##,
        ),
    ] {
        let path = root.join(rel);
        let _ = std::fs::create_dir_all(path.parent().expect("a folder"));
        std::fs::write(&path, text).expect("write a drill file");
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(tree) = app.tree.get() else {
            return bench_quit(&app);
        };
        until(|| {
            app.reconciled.get()
                && [ROW, IMAGE, FOLDER, CODE]
                    .iter()
                    .all(|rel| tree.reveal(rel))
        })
        .await;
        app.open_path(NOTE);
        until(|| app.tab_for(NOTE).is_some()).await;
        glib::timeout_future(Duration::from_millis(600)).await;
        super::git::xtest("move 700 400; focus").await;
        // Screen coordinates: the window at 0,0, its client-side shadow a margin in.
        let (sx, sy) = app.window.surface_transform();
        let screen = |p: graphene::Point| (f64::from(p.x()) + sx, f64::from(p.y()) + sy);
        // A secondary press on the status bar, which does nothing: the first press the window
        // gets under Xvfb starts no drag.
        let bar = app.statusbar.widget();
        let middle = graphene::Point::new(bar.width() as f32 / 2.0, bar.height() as f32 / 2.0);
        if let Some((x, y)) = bar.compute_point(&app.window, &middle).map(screen) {
            super::git::xtest(&format!("move {x} {y}; sleep 0.2; down 3; up 3")).await;
        }
        let pane = app.pane();
        let column = pane.widget().clone();
        let mid = graphene::Point::new(column.width() as f32 / 2.0, column.height() as f32 / 2.0);
        let Some(to) = column.compute_point(&app.window, &mid).map(screen) else {
            return bench_quit(&app);
        };
        let mut failed = false;
        let mut check = |what: &str, ok: bool| {
            println!("bench refer {what} {}", if ok { "ok" } else { "WRONG" });
            failed |= !ok;
        };
        let note = || app.tab_for(NOTE).map(|t| t.text()).unwrap_or_default();
        let front = || app.doc_of(&app.pane()).map(|d| d.key());
        let kept = note();

        // A plain drop over a note opens the row, the shade and the hint up while it hovers.
        // Asked before every drag: opening a file scrolls the tree to its row.
        let seen = drag(&app, row_at(&app, ROW).await, to, false).await;
        println!("bench refer note plain {seen:?} front={:?}", front());
        check(
            "note_plain",
            seen.shade
                && !seen.caret
                && seen.tip.as_deref() == Some("Hold Shift to insert a link")
                && front().as_deref() == Some(ROW)
                && note() == kept,
        );
        if let Some(page) = app.doc_for(ROW).map(|d| d.page().clone()) {
            app.close_page(&page);
        }
        until(|| front().as_deref() == Some(NOTE)).await;

        // With Shift the caret takes the shade's place and the link goes in, the note taking
        // the keyboard.
        let seen = drag(&app, row_at(&app, ROW).await, to, true).await;
        let typed = note();
        let keyboard = app
            .tab_for(NOTE)
            .is_some_and(|tab| app.focused().as_ref() == Some(tab.view.upcast_ref()));
        println!("bench refer note shift {seen:?} text={typed:?} keyboard={keyboard}");
        check(
            "note_shift",
            seen.caret
                && typed.contains("[[target]]")
                && keyboard
                && front().as_deref() == Some(NOTE),
        );

        // A marked set: a link a line, the image embedded, one undo step for both.
        tree.toggle_mark(IMAGE);
        tree.toggle_mark(ROW);
        let before = note();
        drag(&app, row_at(&app, ROW).await, to, true).await;
        let typed = note();
        println!("bench refer set text={typed:?}");
        let both = typed.contains("![[fig.png]]\n[[target]]");
        if let Some(tab) = app.tab_for(NOTE) {
            tab.buffer.undo();
        }
        check("set_shift", both && note() == before);
        tree.clear_marks();

        // A folder: a note follows no link to one, so the text refuses it.
        let before = note();
        let seen = drag(&app, row_at(&app, FOLDER).await, to, true).await;
        println!("bench refer folder {seen:?}");
        check(
            "folder_note",
            seen.began && !seen.caret && note() == before && front().as_deref() == Some(NOTE),
        );

        // A code file: the row's path from the file's folder, a folder's too.
        app.open_path(CODE);
        until(|| front().as_deref() == Some(CODE)).await;
        glib::timeout_future(Duration::from_millis(400)).await;
        let code = || app.tab_for(CODE).map(|t| t.text()).unwrap_or_default();
        let seen = drag(&app, row_at(&app, ROW).await, to, true).await;
        drag(&app, row_at(&app, FOLDER).await, to, true).await;
        println!("bench refer code {seen:?} text={:?}", code());
        check(
            "code_shift",
            seen.tip.as_deref() == Some("Hold Shift to insert a path")
                && code().contains("../target.md")
                && code().contains("../sub"),
        );

        // A notebook: a link in a markdown cell, the path from the notebook in a code cell.
        app.open_path(NOTEBOOK);
        until(|| front().as_deref() == Some(NOTEBOOK)).await;
        let Some(book) = app.notebook_of(&app.pane()) else {
            return bench_quit(&app);
        };
        let mut cells = Vec::new();
        for n in 1..3 {
            book.bench_select(n);
            glib::timeout_future(Duration::from_millis(400)).await;
            let at = Some(book.key_target()).and_then(|v| {
                let middle = graphene::Point::new(v.width() as f32 / 2.0, v.height() as f32 / 2.0);
                v.compute_point(&app.window, &middle)
            });
            let seen = drag(
                &app,
                row_at(&app, ROW).await,
                at.map(screen).unwrap_or(to),
                true,
            )
            .await;
            cells.push((seen.caret, book.bench_source(n)));
        }
        println!("bench refer notebook {cells:?}");
        check(
            "notebook_shift",
            cells[0].0
                && cells[0].1.contains("[[target]]")
                && cells[1].0
                && cells[1].1.contains("target.md"),
        );

        // A shell: the quoted absolute path, typed at the prompt and not run.
        app.open_terminal();
        until(|| app.terminals().len() == 1).await;
        glib::timeout_future(Duration::from_millis(1500)).await;
        drag(&app, row_at(&app, ROW).await, to, true).await;
        glib::timeout_future(Duration::from_millis(500)).await;
        let screen_text = app
            .terminals()
            .first()
            .map(|t| {
                t.view
                    .text_format(vte4::Format::Text)
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();
        let quoted = format!("'{}' ", root_of(&app).join(ROW).display());
        check("shell_shift", screen_text.contains(&quoted));

        // A PDF takes no reference: with Shift the row opens there, as without it.
        app.open_path(PDF);
        until(|| front().as_deref() == Some(PDF)).await;
        glib::timeout_future(Duration::from_millis(400)).await;
        let seen = drag(&app, row_at(&app, ROW).await, to, true).await;
        println!("bench refer pdf {seen:?} front={:?}", front());
        check("pdf_shift", !seen.caret && front().as_deref() == Some(ROW));

        if failed {
            std::process::exit(1);
        }
        bench_quit(&app);
    });
}

/// The middle of `rel`'s row on the screen, scrolled into view first: opening a file scrolls the
/// tree to its own row.
async fn row_at(app: &Rc<App>, rel: &str) -> (f64, f64) {
    let Some(tree) = app.tree.get() else {
        return (0.0, 0.0);
    };
    tree.reveal(rel);
    glib::timeout_future(Duration::from_millis(300)).await;
    let (sx, sy) = app.window.surface_transform();
    super::folders::centre(app, tree, rel).map_or((0.0, 0.0), |p| {
        (f64::from(p.x()) + sx, f64::from(p.y()) + sy)
    })
}

/// The vault's folder on the machine holding it, which a shell is given paths in.
fn root_of(app: &App) -> std::path::PathBuf {
    app.vault().map(|v| v.root()).unwrap_or_default()
}

/// What a drag saw: whether it began, whether the active pane's shade and caret came up, and the
/// last hint it gave.
#[derive(Debug, Default)]
struct Seen {
    began: bool,
    shade: bool,
    caret: bool,
    tip: Option<String>,
}

/// Drag with XTEST from `from` to `to`, out past the drag threshold first, Shift going down over
/// the pane when `shift` and up after the release.
async fn drag(app: &Rc<App>, from: (f64, f64), to: (f64, f64), shift: bool) -> Seen {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let watch = glib::timeout_add_local(Duration::from_millis(20), {
        let (app, pane, seen) = (app.clone(), app.pane(), seen.clone());
        move || {
            let (shade, caret, tip) = pane.drop_marks();
            let mut seen = seen.borrow_mut();
            seen.began |= app
                .shell
                .upgrade()
                .is_some_and(|shell| shell.tree_drag.borrow().is_some());
            seen.shade |= shade;
            seen.caret |= caret;
            if tip.is_some() {
                seen.tip = tip;
            }
            glib::ControlFlow::Continue
        }
    });
    let ((x, y), out) = (from, from.0 + 30.0);
    let step = |i: i32| {
        let f = f64::from(i) / 10.0;
        format!(
            "move {} {}; sleep 0.03; ",
            out + (to.0 - out) * f,
            y + (to.1 - y) * f
        )
    };
    // Shift goes down over the middle of the pane, once the hint there has shown.
    let first: String = (1..=8).map(step).collect();
    let rest: String = (9..=10).map(step).collect();
    let (down, up) = match shift {
        true => ("keydown shift; sleep 0.1; ", "; keyup shift"),
        false => ("", ""),
    };
    super::git::xtest(&format!(
        "move {x} {y}; sleep 0.3; down; sleep 0.1; move {} {y}; sleep 0.05; move {} {y}; \
         sleep 0.05; move {out} {y}; sleep 0.1; {first}{down}{rest}sleep 0.3; up{up}",
        x + 10.0,
        x + 20.0
    ))
    .await;
    watch.remove();
    glib::timeout_future(Duration::from_millis(300)).await;
    seen.take()
}
