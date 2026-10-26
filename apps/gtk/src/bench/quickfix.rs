//! Quick Fix against `build-aux/fake-lsp.py --actions`: the menu, and each kind of action applied,
//! in a C file and in a notebook's code cell; and a note's own fix for a link to nothing.

use super::*;
use accent_core::notebook::{self, Edit, Kind};

/// The note the drill writes and fixes, and the file one of the fixes reaches into.
const FIX: &str = "int a = oops;\nint b = 1 + 2;\n";
const OTHER: &str = "int other;\n";

/// `ACCENT_BENCH_QUICKFIX=<rel>,<ipynb>` writes `<rel>` and `other.c` in a scratch vault under
/// `/tmp`, opens `<rel>` with the caret on `oops` once its error is in, and prints the Quick Fix
/// menu's rows. Then, a fresh menu each time, it picks each row: the preferred fix and the
/// resolved refactoring and the command each change the text and are undone in one step, the
/// fix reaching into `other.c` writes that file on disk, and the one creating `oops.h` makes it,
/// and is refused when picked again. The hover's fixes over `oops` follow.
/// Then `<ipynb>`, `b = 1 + 2` and `a = oops` in two code cells, the same five rows picked in the
/// second: the two inside it change it, the three reaching into the first cell and other files do
/// nothing. Last, `link.md` links to a note that is not there, and its one fix opens New File.
pub(super) fn bench_quickfix(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_QUICKFIX");
    let Some((rel, ipynb)) = arg.split_once(',') else {
        eprintln!("ACCENT_BENCH_QUICKFIX=<rel>,<ipynb>");
        return bench_quit(app);
    };
    let (app, rel, ipynb) = (app.clone(), rel.to_string(), ipynb.to_string());
    glib::spawn_future_local(async move {
        bench_fixes(&app, &rel).await;
        bench_cell_fixes(&app, &ipynb).await;
        bench_note_fix(&app).await;
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
    // The file-creating fix last a second time, with the file there.
    for (n, pick) in [0u32, 1, 2, 3, 4, 2].into_iter().enumerate() {
        if n == 5 {
            app.toasts.dismiss_all();
            glib::timeout_future(Duration::from_millis(500)).await;
        }
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
        let read_other = || {
            let read = |rel| std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
            (read("other.c"), read("oops.h"))
        };
        let (before, other_before) = (tab.text(), read_other());
        let _ = tab
            .view
            .activate_action("fix.apply", Some(&pick.to_variant()));
        menu.popdown();
        for _ in 0..50 {
            let refused = n == 5 && super::compare::bench_toast(app).is_some();
            if tab.text() != before || read_other() != other_before || refused {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        if n == 5 {
            let said = super::compare::bench_toast(app);
            println!("bench quickfix again={pick} said={said:?}");
            continue;
        }
        let (other, made) = read_other();
        println!(
            "bench quickfix applied={pick} text={:?} other={other:?} made={made:?}",
            tab.text()
        );
        if tab.text() != before {
            tab.buffer.undo();
            println!("bench quickfix undone={pick} text={:?}", tab.text());
        }
        crate::lang::flush(tab.clone()).await;
    }
    let on = accent_api::Pos {
        line: 0,
        character: 9,
    };
    let hover = crate::hover::fixes_at(tab.clone(), on).await;
    let titles: Vec<String> = hover.into_iter().map(|a| a.title).collect();
    println!("bench quickfix hover {titles:?}");
}

async fn bench_cell_fixes(app: &Rc<App>, rel: &str) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let Ok(mut nb) = notebook::read(rel, notebook::EMPTY_IPYNB) else {
        return;
    };
    let id = nb.cells()[0].id();
    nb.apply(Edit::Source {
        id,
        text: "b = 1 + 2".into(),
    });
    if let Some(cell) = nb.new_cell(Kind::Code, "a = oops") {
        nb.apply(Edit::Insert { at: 1, cell });
    }
    if let Err(e) = vault.save(rel, &nb.to_text(), None) {
        return println!("bench quickfix write_failed {rel} {e:?}");
    }
    app.open_path(rel);
    let mut found = None;
    for _ in 0..300 {
        let tab = app.doc_for(rel).and_then(|d| d.notebook().cloned());
        if let Some(tab) = tab.filter(|t| {
            t.bench_diagnostics()
                .get(1)
                .is_some_and(|c| !c.0.is_empty())
        }) {
            found = Some(tab);
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let Some(tab) = found else {
        return println!("bench quickfix cell_no_diagnostics {rel}");
    };
    let sources =
        |tab: &Rc<crate::notebook::NotebookTab>| [tab.bench_source(0), tab.bench_source(1)];
    for pick in 0..5u32 {
        tab.bench_select(1);
        let Some(cell) = tab.bench_language_editor(1) else {
            return println!("bench quickfix no_cell_editor");
        };
        let view = cell.view().clone();
        let mut caret = view.buffer().start_iter();
        caret.set_line_offset(5);
        view.buffer().place_cursor(&caret);
        app.quick_fix();
        let Some(menu) = menu_on(&view).await else {
            return println!("bench quickfix cell_no_menu {pick}");
        };
        if pick == 0 {
            let rows = menu.menu_model().map(|m| fileops::labels(&m));
            println!("bench quickfix cell_menu {rows:?}");
        }
        let before = sources(&tab);
        let _ = view.activate_action("fix.apply", Some(&pick.to_variant()));
        menu.popdown();
        for _ in 0..20 {
            if sources(&tab) != before {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        println!(
            "bench quickfix cell_applied={pick} cells={:?}",
            sources(&tab)
        );
        if sources(&tab) != before {
            tab.undo();
            println!(
                "bench quickfix cell_undone={pick} cells={:?}",
                sources(&tab)
            );
        }
        cell.clone().flush().await;
    }
}

/// Quick Fix on a link to a note that is not there offers to create it, and picking that opens
/// New File with the path typed in, as following the link does.
async fn bench_note_fix(app: &Rc<App>) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let rel = "link.md";
    if let Err(e) = vault.save(rel, "See [[Nowhere]].\n", None) {
        return println!("bench quickfix write_failed {rel} {e:?}");
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
        return println!("bench quickfix note_no_hint {rel}");
    };
    let on = accent_api::Pos {
        line: 0,
        character: 8,
    };
    let hover = crate::hover::fixes_at(tab.clone(), on).await;
    let titles: Vec<String> = hover.into_iter().map(|a| a.title).collect();
    println!("bench quickfix note_hover {titles:?}");
    tab.view.grab_focus();
    tab.buffer.place_cursor(&tab.buffer.iter_at_offset(8));
    app.quick_fix();
    let Some(menu) = shown_menu(&tab).await else {
        return println!("bench quickfix note_no_menu");
    };
    let rows = menu.menu_model().map(|m| fileops::labels(&m));
    println!("bench quickfix note_menu {rows:?}");
    let _ = tab
        .view
        .activate_action("fix.apply", Some(&0u32.to_variant()));
    menu.popdown();
    super::style::bench_new_file(app, "create_note").await;
}

/// The Quick Fix menu once it is up on `tab`'s view.
async fn shown_menu(tab: &Rc<Tab>) -> Option<gtk::PopoverMenu> {
    menu_on(&tab.view).await
}

/// The Quick Fix menu once it is up on `view`.
async fn menu_on(view: &sourceview5::View) -> Option<gtk::PopoverMenu> {
    for _ in 0..50 {
        let mut child = view.first_child();
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
