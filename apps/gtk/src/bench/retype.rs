//! A rename to another extension: the open tab follows the file as what its new name makes it.

use super::*;

/// `ACCENT_BENCH_RETYPE=1` opens `retype.md` before `other.md` and renames it through the move
/// path to `.org` and `.py` (each the same kind of page as before, so the same tab), `.txt` (a
/// note becoming code, a tab of its own) and `.png` (no text at all), typing into the text tab
/// before each. After each prints `bench retype <name> doc=<kind> flavour=<flavour>
/// language=<id> same=<same tab> undo=<undo kept> at=<place in the bar>`.
pub(super) fn bench_retype(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(ops) = app.ops().cloned() else {
            println!("bench retype no_vault");
            return bench_quit(&app);
        };
        until("retype", "reconcile", 30_000, || app.reconciled.get()).await;
        for rel in ["retype.md", "other.md"] {
            app.open_path(rel);
            until("retype", rel, 10_000, || app.tab_for(rel)).await;
        }
        let mut from = "retype.md".to_string();
        for to in ["retype.org", "retype.txt", "retype.py", "retype.png"] {
            let before = app.tab_for(&from);
            if let Some(tab) = &before {
                tab.buffer.begin_user_action();
                tab.buffer.insert(&mut tab.buffer.end_iter(), "typed\n");
                tab.buffer.end_user_action();
            }
            crate::fileops::move_all(&ops, vec![(from.clone(), to.to_string())]);
            let landed = || {
                app.doc_for(to)
                    .filter(|doc| !matches!(doc, Doc::Loading(_)))
            };
            let Some(doc) = until("retype", to, 10_000, landed).await else {
                return bench_quit(&app);
            };
            let tab = doc.tab();
            let kind = match &doc {
                Doc::Text(_) => "text",
                Doc::Image(_) => "image",
                Doc::Status(_) => "status",
                _ => "other",
            };
            let at = app
                .pane_of(doc.page())
                .map(|pane| pane.tabs.page_position(doc.page()));
            println!(
                "bench retype {to} doc={kind} flavour={:?} language={:?} same={} undo={} at={at:?}",
                tab.map(|t| t.flavour()),
                tab.map(|t| crate::lang::language_id(t)),
                tab.zip(before.as_ref())
                    .is_some_and(|(a, b)| Rc::ptr_eq(a, b)),
                tab.is_some_and(|t| t.buffer.can_undo()),
            );
            from = to.to_string();
        }
        bench_quit(&app);
    });
}
