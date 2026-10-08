//! Go to Symbol: the palette's `@` mode over the index's declarations.

use super::*;

/// `ACCENT_BENCH_SWITCHER=symbol:<query>` waits for the vault to be indexed, opens Filter by Tag
/// and prints its title and row count; then types `@<query>` into Go to File, and `<query>` into
/// Go to Symbol, printing each dialog's title and first rows (`symbol_row <name> <container>
/// <path>:<line>`); last it picks the first and prints the file and the caret it landed on.
pub(super) fn bench_symbol(app: &Rc<App>, query: &str) {
    let (app, query) = (app.clone(), query.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        let _ = WidgetExt::activate_action(&app.window, "win.palette-tags", None);
        glib::timeout_future(Duration::from_millis(500)).await;
        let rows = app.window.visible_dialog().and_then(|d| {
            let list = find_widget(d.upcast_ref(), &|w| w.is::<gtk::ListView>());
            let rows = list.and_downcast::<gtk::ListView>()?.model()?.n_items();
            d.close();
            Some((d.title().to_string(), rows))
        });
        println!("bench symbol_tags {rows:?}");
        glib::timeout_future(Duration::from_millis(500)).await;
        for (action, typed) in [
            ("win.palette-files", format!("@{query}")),
            ("win.palette-symbols", query.clone()),
        ] {
            let _ = WidgetExt::activate_action(&app.window, action, None);
            let dialog = app.window.visible_dialog();
            let Some(entry) = dialog
                .as_ref()
                .and_then(|d| find_search_entry(d.upcast_ref()))
            else {
                return bench_quit(&app);
            };
            entry.set_text(&typed);
            glib::timeout_future(Duration::from_millis(1500)).await;
            println!(
                "bench symbol_title {action} {:?}",
                dialog.as_ref().map(|d| d.title().to_string())
            );
            symbol_rows(&app);
            if action == "win.palette-symbols" {
                entry.emit_activate();
            } else if let Some(dialog) = dialog {
                dialog.close();
            }
            glib::timeout_future(Duration::from_millis(1000)).await;
        }
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

/// The palette's first rows, each declaration's name, type and place.
fn symbol_rows(app: &Rc<App>) {
    let list = app.window.visible_dialog().and_then(|d| {
        find_widget(d.upcast_ref(), &|w| w.is::<gtk::ListView>()).and_downcast::<gtk::ListView>()
    });
    let Some(model) = list.and_then(|l| l.model()) else {
        return;
    };
    for i in 0..model.n_items().min(5) {
        let Some(boxed) = model.item(i).and_downcast::<glib::BoxedAnyObject>() else {
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
}
