//! Run Command over a note and over a PDF: which commands it lists for each.

use super::*;

/// Commands that mean something over a PDF alone.
const PDF_ONLY: [&str; 8] = [
    "win.export-pdf",
    "win.pdf-fit-width",
    "win.pdf-next-page",
    "win.pdf-add-page-after",
    "win.pdf-pen",
    "win.pdf-undo",
    "win.pdf-invert",
    "win.pdf-copy",
];

/// Commands that mean something over a markdown note alone.
const NOTE_ONLY: [&str; 7] = [
    "win.export-html",
    "win.open-in-browser",
    "win.view-mode",
    "win.toggle-task",
    "win.insert-sketch",
    "win.fold",
    "win.duplicate-line",
];

/// `ACCENT_BENCH_TABS=commands` writes `commands.md` and a one-page `commands.pdf` at the vault
/// root, opens Run Command over each, and prints `bench listed <kind> rows=<n> other=[…]
/// missing=[…]`: the other kind's commands among its rows, and its own not among them. Exits 1
/// where either is not empty. Writes into the vault: a scratch one under `/tmp`.
pub(super) fn bench_listed(app: &Rc<App>) {
    scratch_only(app, "ACCENT_BENCH_TABS=commands");
    let (note, pdf) = ("commands.md", "commands.pdf");
    let root = app.root();
    let written = std::fs::write(root.join(note), "# Commands\n").map_err(|e| e.to_string());
    let written = written.and_then(|()| {
        let surface = cairo::PdfSurface::new(595.0, 842.0, root.join(pdf));
        let surface = surface.map_err(|e| e.to_string())?;
        cairo::Context::new(&surface)
            .and_then(|cr| cr.show_page())
            .map_err(|e| e.to_string())?;
        surface.finish();
        Ok(())
    });
    if let Err(e) = written {
        println!("bench listed wrote=false {e}");
        std::process::exit(1);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        app.open_path(note);
        // Its tab is up at once, and its document once read.
        for _ in 0..50 {
            if app.active_key().as_deref() == Some(note) && app.tab_for(note).is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let over_note = rows(&app).await;
        if super::pdf::opened(&app, pdf).await.is_none() {
            println!("bench listed no_pdf");
            std::process::exit(1);
        }
        let over_pdf = rows(&app).await;
        let mut right = true;
        for (kind, rows, own, other) in [
            ("note", &over_note, &NOTE_ONLY[..], &PDF_ONLY[..]),
            ("pdf", &over_pdf, &PDF_ONLY[..], &NOTE_ONLY[..]),
        ] {
            let listed = |a: &&str| rows.iter().any(|row| row == a);
            let other: Vec<&str> = other.iter().copied().filter(listed).collect();
            let missing: Vec<&str> = own.iter().copied().filter(|a| !listed(a)).collect();
            // A full page may have cut a command off, which says nothing either way.
            let full = rows.len() >= palette::MAX_RESULTS;
            println!(
                "bench listed {kind} rows={} other={other:?} missing={missing:?}",
                rows.len()
            );
            right &= other.is_empty() && missing.is_empty() && !full;
        }
        if !right {
            std::process::exit(1);
        }
        bench_quit(&app);
    });
}

/// The actions of Run Command's rows, opened on an empty query over the tab in front.
async fn rows(app: &Rc<App>) -> Vec<String> {
    let _ = WidgetExt::activate_action(&app.window, "win.palette-commands", None);
    glib::timeout_future(Duration::from_millis(400)).await;
    let Some(dialog) = app.window.visible_dialog() else {
        return Vec::new();
    };
    let list = find_widget(dialog.upcast_ref(), &|w| w.is::<gtk::ListView>());
    let model = list.and_downcast::<gtk::ListView>().and_then(|l| l.model());
    let mut actions = Vec::new();
    for i in 0..model.as_ref().map_or(0, |m| m.n_items()) {
        let boxed = model.as_ref().and_then(|m| m.item(i));
        let Some(boxed) = boxed.and_downcast::<glib::BoxedAnyObject>() else {
            continue;
        };
        if let palette::Item::Command { action, .. } = &**boxed.borrow::<Rc<palette::Item>>() {
            actions.push(action.clone());
        }
    }
    dialog.force_close();
    glib::timeout_future(Duration::from_millis(200)).await;
    actions
}
