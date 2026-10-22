//! Quick Fix against `build-aux/fake-lsp.py --actions`: the menu, and each kind of action applied.

use super::*;

/// The note the drill writes and fixes, and the file one of the fixes reaches into.
const FIX: &str = "int a = oops;\nint b = 1 + 2;\n";
const OTHER: &str = "int other;\n";

/// `ACCENT_BENCH_QUICKFIX=<rel>` writes `<rel>` and `other.c` in a scratch vault under `/tmp`,
/// opens `<rel>` with the caret on `oops` once its error is in, and prints the Quick Fix menu's
/// rows. Then, a fresh menu each time, it picks each row: the preferred fix and the resolved
/// refactoring and the command each change the text and are undone in one step, and the fix
/// reaching into `other.c` writes that file on disk.
pub(super) fn bench_quickfix(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_QUICKFIX");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        bench_fixes(&app, &rel).await;
        bench_quit(&app);
    });
}

async fn bench_fixes(app: &Rc<App>, rel: &str) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    for (file, text) in [(rel, FIX), ("other.c", OTHER)] {
        if let Err(e) = vault.save(file, text, None) {
            return println!("bench quickfix write_failed {file} {e:?}");
        }
    }
    app.open_path(rel);
    let mut tab = None;
    for _ in 0..300 {
        tab = app
            .open_tabs()
            .into_iter()
            .find(|tab| tab.rel() == rel && !tab.diagnostics().is_empty());
        if tab.is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let Some(tab) = tab else {
        return println!("bench quickfix no_diagnostics {rel}");
    };
    tab.view.grab_focus();
    for pick in 0..4u32 {
        let mut caret = tab
            .buffer
            .iter_at_line(0)
            .unwrap_or(tab.buffer.start_iter());
        caret.set_line_offset(9);
        tab.buffer.place_cursor(&caret);
        app.quick_fix();
        let Some(menu) = shown_menu(&tab).await else {
            return println!("bench quickfix no_menu {pick}");
        };
        if pick == 0 {
            let rows = menu.menu_model().map(|m| fileops::labels(&m));
            println!("bench quickfix menu {rows:?}");
        }
        let read_other = || std::fs::read_to_string(app.root().join("other.c")).unwrap_or_default();
        let (before, other_before) = (tab.text(), read_other());
        let _ = tab
            .view
            .activate_action("fix.apply", Some(&pick.to_variant()));
        menu.popdown();
        for _ in 0..50 {
            if tab.text() != before || read_other() != other_before {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let other = read_other();
        println!(
            "bench quickfix applied={pick} text={:?} other={other:?}",
            tab.text()
        );
        if tab.text() != before {
            tab.buffer.undo();
            println!("bench quickfix undone={pick} text={:?}", tab.text());
        }
        crate::lang::flush(tab.clone()).await;
    }
}

/// The Quick Fix menu once it is up on `tab`'s view.
async fn shown_menu(tab: &Rc<Tab>) -> Option<gtk::PopoverMenu> {
    for _ in 0..50 {
        let mut child = tab.view.first_child();
        while let Some(widget) = child {
            if let Some(menu) = widget.downcast_ref::<gtk::PopoverMenu>()
                && menu.is_visible()
            {
                return Some(menu.clone());
            }
            child = widget.next_sibling();
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    None
}
