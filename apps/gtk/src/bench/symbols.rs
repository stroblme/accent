//! Go to Symbol: the palette's `@` mode over the index's declarations.

use super::*;

/// `ACCENT_BENCH_SWITCHER=symbol:<query>` waits for the vault to be indexed, types `@<query>` into
/// Go to File, and prints the dialog's title and its first rows (`symbol_row <name> <container>
/// <path>:<line>`); then picks the first and prints the file and the caret it landed on.
pub(super) fn bench_symbol(app: &Rc<App>, query: &str) {
    let (app, query) = (app.clone(), query.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        let dialog = app.window.visible_dialog();
        let Some(entry) = dialog
            .as_ref()
            .and_then(|d| find_search_entry(d.upcast_ref()))
        else {
            return bench_quit(&app);
        };
        entry.set_text(&format!("@{query}"));
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!(
            "bench symbol_title {:?}",
            dialog.map(|d| d.title().to_string())
        );
        let list = app.window.visible_dialog().and_then(|d| {
            find_widget(d.upcast_ref(), &|w| w.is::<gtk::ListView>())
                .and_downcast::<gtk::ListView>()
        });
        let model = list.and_then(|l| l.model());
        for i in 0..model.as_ref().map_or(0, |m| m.n_items().min(5)) {
            let Some(boxed) = model
                .as_ref()
                .and_then(|m| m.item(i))
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                continue;
            };
            if let crate::palette::Item::Symbol(s) = &**boxed.borrow::<Rc<crate::palette::Item>>() {
                println!(
                    "bench symbol_row {} {} {}:{}",
                    s.name,
                    s.container.as_deref().unwrap_or("-"),
                    s.rel_path,
                    s.line
                );
            }
        }
        entry.emit_activate();
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active() else {
            println!("bench symbol_opened none");
            return bench_quit(&app);
        };
        let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
        let (mut start, mut end) = (caret, caret);
        start.set_line_offset(0);
        end.forward_to_line_end();
        println!(
            "bench symbol_opened {} caret={}:{} line={:?}",
            tab.rel(),
            caret.line() + 1,
            caret.line_offset(),
            tab.buffer.text(&start, &end, false).trim()
        );
        bench_quit(&app);
    });
}
