//! `ACCENT_BENCH_NOTEBOOK_LSP=<ipynb>,<marimo .py>`: a notebook's code cells answered through
//! the notebook's one document, by `build-aux/fake-lsp.py --notebook` as the vault's `python3`
//! server in `make drills`. It writes both notebooks into a scratch vault under /tmp and prints
//! the key the program opened under; the diagnostics in their cells, a cell's shown value not
//! called unused, and a cell two screens down holding its share until its editor is built; a
//! hover; Go to Definition into another cell; a completion whose import into another cell is
//! left out; signature help; the hover again after a line typed in above; and Go to Definition
//! in a marimo notebook.

use super::*;
use crate::notebook::NotebookTab;
use accent_api::Pos;
use accent_core::notebook::{self, Edit, Kind};

/// Code cells after the first three, enough that the last is far below the view.
const FILLER: usize = 60;

pub(super) fn bench_notebook_lsp(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_NOTEBOOK_LSP");
    let Some((ipynb, marimo)) = arg.split_once(',') else {
        eprintln!("ACCENT_BENCH_NOTEBOOK_LSP=<ipynb>,<marimo .py>");
        return bench_quit(app);
    };
    let (app, ipynb, marimo) = (app.clone(), ipynb.to_string(), marimo.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..300 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        if write(&app, &ipynb, &marimo) {
            ipynb_drill(&app, &ipynb).await;
            marimo_drill(&app, &marimo).await;
        }
        bench_quit(&app);
    });
}

/// The two notebooks: an `.ipynb` of a markdown cell, `import os\nx = 1`, `%matplotlib
/// inline\ny = x + oops`, `x`, the filler and `z = oops`; and a marimo notebook of `x = 1` and
/// `y = x + 1`.
fn write(app: &Rc<App>, ipynb: &str, marimo: &str) -> bool {
    let Some(vault) = app.vault().cloned() else {
        return false;
    };
    let Ok(mut nb) = notebook::read(ipynb, notebook::EMPTY_IPYNB) else {
        return false;
    };
    let id = nb.cells()[0].id();
    nb.apply(Edit::Source {
        id,
        text: "import os\nx = 1".into(),
    });
    let mut cells = vec![
        (0, Kind::Markdown, "# T"),
        (2, Kind::Code, "%matplotlib inline\ny = x + oops"),
        (3, Kind::Code, "x"),
    ];
    cells.extend((0..FILLER).map(|n| (4 + n, Kind::Code, "pass")));
    cells.push((4 + FILLER, Kind::Code, "z = oops"));
    for (at, kind, source) in cells {
        if let Some(cell) = nb.new_cell(kind, source) {
            nb.apply(Edit::Insert { at, cell });
        }
    }
    let marimo_text = "import marimo\n\napp = marimo.App()\n\n\n@app.cell\ndef _():\n    x = 1\n    \
                       return (x,)\n\n\n@app.cell\ndef _(x):\n    y = x + 1\n    return\n\n\n\
                       if __name__ == \"__main__\":\n    app.run()\n";
    for (rel, text) in [(ipynb, nb.to_text()), (marimo, marimo_text.to_string())] {
        if let Err(e) = vault.save(rel, &text, None) {
            println!("bench notebook_lsp write_failed {rel} {e:?}");
            return false;
        }
    }
    true
}

/// `rel` opened, once its server has answered and said what is wrong with it.
async fn opened(app: &Rc<App>, rel: &str) -> Option<Rc<NotebookTab>> {
    app.open_path(rel);
    for _ in 0..300 {
        let tab = app.doc_for(rel).and_then(|d| d.notebook().cloned());
        if let Some(tab) = tab.filter(|t| t.bench_supported()) {
            // The first publish follows the opening.
            glib::timeout_future(Duration::from_millis(300)).await;
            return Some(tab);
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    println!("bench notebook_lsp no_server {rel}");
    None
}

async fn ipynb_drill(app: &Rc<App>, rel: &str) {
    let Some(tab) = opened(app, rel).await else {
        return;
    };
    println!(
        "bench notebook_lsp open key={}",
        tab.document_key().unwrap_or_default()
    );
    let cells = tab.bench_diagnostics();
    let held = |n: usize| cells[n].0.len().to_string();
    let lines = |n: usize| match &cells[n].1 {
        Some(lines) if !lines.is_empty() => format!("{lines:?}"),
        _ => "-".to_string(),
    };
    let unused = cells
        .iter()
        .flat_map(|(items, _)| items)
        .filter(|d| d.code.as_deref() == Some("reportUnusedExpression"))
        .count();
    println!(
        "bench notebook_lsp diagnostics cells={},{},{} underlined={},{},{} unused={unused} \
         counts={:?}",
        held(0),
        held(1),
        held(2),
        lines(0),
        lines(1),
        lines(2),
        tab.diagnostics_counts(),
    );
    let last = tab.cell_count() - 1;
    let (waiting, built) = cells.last().cloned().unwrap_or_default();
    let painted = tab
        .bench_language_editor(last)
        .map_or(0, |editor| editor.diagnostics().len());
    println!(
        "bench notebook_lsp lazy built={} waiting={} painted_on_build={painted}",
        u8::from(built.is_some()),
        waiting.len(),
    );

    // The status count takes them out of every cell's text and puts them back.
    app.sync_status();
    let count = app.statusbar.facts_control();
    let underlined = |tab: &Rc<NotebookTab>| {
        let cells = tab.bench_diagnostics();
        cells
            .iter()
            .filter_map(|(_, lines)| lines.as_ref())
            .map(Vec::len)
            .sum::<usize>()
    };
    let pressable = count.can_target();
    count.emit_clicked();
    let hidden = underlined(&tab);
    count.emit_clicked();
    println!(
        "bench notebook_lsp press pressable={pressable} tooltip={:?} hidden_underlined={hidden} \
         back={}",
        count.tooltip_text().map(|t| t.to_string()),
        underlined(&tab),
    );

    let Some(cell) = tab.bench_language_editor(2) else {
        return;
    };
    let at = |editor: &Rc<dyn crate::lang::Editor>, line, character| {
        crate::lang::iter_at(&editor.view().buffer(), Pos { line, character })
    };
    let hover = crate::hover::markup_at(cell.clone(), at(&cell, 1, 4)).await;
    println!("bench notebook_lsp hover {:?}", hover.unwrap_or_default());

    tab.bench_select(2);
    cell.view().buffer().place_cursor(&at(&cell, 1, 4));
    app.go_to_definition();
    let landed = moved_to(&tab, (2, 1, 4)).await;
    println!("bench notebook_lsp definition {landed}");

    complete(&tab, &cell).await;

    // `(` asks for the call's signature.
    let buffer = cell.view().buffer();
    buffer.place_cursor(&buffer.end_iter());
    typed(&buffer, "(");
    let mut shown = None;
    for _ in 0..60 {
        glib::timeout_future(Duration::from_millis(50)).await;
        shown = cell.signature().shown();
        if shown.is_some() {
            break;
        }
    }
    cell.signature().dismiss();
    println!(
        "bench notebook_lsp signature {:?}",
        shown.unwrap_or_default()
    );

    // A line typed in above moves the cell down the program.
    if let Some(first) = tab.bench_language_editor(1) {
        let buffer = first.view().buffer();
        buffer.place_cursor(&buffer.start_iter());
        typed(&buffer, "# note\n");
    }
    let hover = crate::hover::markup_at(cell.clone(), at(&cell, 1, 4)).await;
    println!(
        "bench notebook_lsp moved hover {:?}",
        hover.unwrap_or_default()
    );
}

/// `.` typed at the end of `cell`, the first row taken, its snippet's walk ended: the rows, the
/// line written and whether the import the server adds to the top went into the first cell.
async fn complete(tab: &Rc<NotebookTab>, cell: &Rc<dyn crate::lang::Editor>) {
    tab.bench_select(2);
    // The popup is made in an idle once the cell is selected.
    glib::timeout_future(Duration::from_millis(50)).await;
    let Some(session) = cell.completion() else {
        return println!("bench notebook_lsp complete no_popup");
    };
    let buffer = cell.view().buffer();
    buffer.place_cursor(&buffer.end_iter());
    cell.view().grab_focus();
    typed(&buffer, ".");
    super::complete::bench_settle(&session).await;
    let (rows, _) = session.rows();
    session.press(gdk::Key::Down, gdk::ModifierType::empty());
    session.press(gdk::Key::Return, gdk::ModifierType::empty());
    cell.end_snippet();
    super::complete::bench_settle(&session).await;
    // The resolve that brings the import back.
    glib::timeout_future(Duration::from_millis(500)).await;
    let line = buffer
        .text(
            &buffer.iter_at_line(1).unwrap_or_else(|| buffer.end_iter()),
            &buffer.end_iter(),
            true,
        )
        .to_string();
    let written = (0..tab.cell_count()).any(|n| tab.bench_source(n).contains("#include"));
    println!(
        "bench notebook_lsp complete rows={} text={line:?} import_written={}",
        rows.join(","),
        u8::from(written)
    );
}

/// `text` typed at the caret of `buffer`, one user action a character.
fn typed(buffer: &gtk::TextBuffer, text: &str) {
    for c in text.chars() {
        buffer.begin_user_action();
        buffer.insert_at_cursor(&c.to_string());
        buffer.end_user_action();
    }
}

/// Where the selection and its caret went from `from`, once they moved: `cell=N at=L:C`.
async fn moved_to(tab: &Rc<NotebookTab>, from: (usize, i32, i32)) -> String {
    let mut at = Some(from);
    for _ in 0..60 {
        glib::timeout_future(Duration::from_millis(50)).await;
        at = tab.bench_caret();
        if at != Some(from) {
            break;
        }
    }
    match at {
        Some((cell, line, column)) => format!("cell={cell} at={line}:{column}"),
        None => "none".to_string(),
    }
}

async fn marimo_drill(app: &Rc<App>, rel: &str) {
    let Some(tab) = opened(app, rel).await else {
        return;
    };
    let Some(cell) = tab.bench_language_editor(1) else {
        return;
    };
    tab.bench_select(1);
    let buffer = cell.view().buffer();
    buffer.place_cursor(&crate::lang::iter_at(
        &buffer,
        Pos {
            line: 0,
            character: 4,
        },
    ));
    app.go_to_definition();
    let landed = moved_to(&tab, (1, 0, 4)).await;
    println!(
        "bench notebook_lsp marimo key={} definition {landed}",
        tab.document_key().unwrap_or_default()
    );
}
