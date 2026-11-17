//! The style editor on the sample: Edit Style opens a tab of the cell's style an item a line,
//! what is typed there reaches the cell once typing pauses, a change elsewhere shows in the tab,
//! Edit Style again comes back to it, completion offers keys and values, the star sets and clears
//! the Default Style, and the tab closes with its cell.

use super::*;

/// Probe the style editor on cell `a` of the sample at `rel`, created if absent.
pub(super) fn style(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        app.show_pane("properties");
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram style no_tab");
            return bench_quit(&app);
        };
        let pane = tab.properties();
        let button = |tooltip: &str| {
            find_widget(&pane, &|w| {
                w.is::<gtk::Button>()
                    && w.tooltip_text()
                        .is_some_and(|t| t.starts_with(tooltip) && w.is_visible())
            })
            .and_downcast::<gtk::Button>()
        };
        let star = || {
            let star = button("Set Default Style").or_else(|| button("Clear Default Style"));
            star.map(|b| {
                (
                    b.icon_name().unwrap_or_default(),
                    b.tooltip_text().unwrap_or_default(),
                )
            })
        };
        let style = |id: &str| tab.file().pages[0].cell(id).map(|c| c.style.to_string());
        let editors = || {
            let docs = app.docs.borrow();
            docs.iter()
                .filter_map(|d| d.tab().filter(|t| t.is_scratch()).cloned())
                .collect::<Vec<_>>()
        };
        tab.select(vec!["a".to_string()]);
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench diagram style star {:?}", star());

        let Some(edit) = button("Edit Style") else {
            println!("bench diagram style no_button Edit Style");
            return bench_quit(&app);
        };
        edit.emit_clicked();
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(editor) = editors().into_iter().next() else {
            println!("bench diagram style no_editor");
            return bench_quit(&app);
        };
        println!(
            "bench diagram style opened title={:?} front={} text={:?}",
            editor.page.title(),
            app.active().is_some_and(|t| Rc::ptr_eq(&t, &editor)),
            editor.text()
        );

        // Completion: a key at the start of a line, a value after its `=`.
        let session = crate::completion::session(&editor);
        for line in ["\nfil", "\nshape=ell"] {
            editor.buffer.place_cursor(&editor.buffer.end_iter());
            editor.buffer.insert_at_cursor(line);
            if let Some(session) = &session {
                session.ask(None);
            }
            glib::timeout_future(Duration::from_millis(200)).await;
            let rows = session.as_ref().map(|s| s.rows().0).unwrap_or_default();
            println!(
                "bench diagram style complete {:?} rows={:?}",
                line.trim(),
                &rows[..rows.len().min(2)]
            );
            let end = editor.buffer.end_iter();
            let mut from = end;
            from.backward_chars(line.chars().count() as i32);
            editor
                .buffer
                .delete(&mut from, &mut editor.buffer.end_iter());
        }
        glib::timeout_future(Duration::from_millis(700)).await;
        println!(
            "bench diagram style untouched a={:?} history={:?}",
            style("a"),
            tab.history()
        );

        // A key typed, then the pause that applies it as one step.
        let mut end = editor.buffer.end_iter();
        editor.buffer.insert(&mut end, "\nshadow=1");
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench diagram style typing a={:?}", style("a"));
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench diagram style applied a={:?} history={:?}",
            style("a"),
            tab.history()
        );

        // A change elsewhere shows in the tab: the diagram's Undo, then its Redo.
        tab.undo();
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench diagram style undone text={:?}", editor.text());
        tab.redo();
        glib::timeout_future(Duration::from_millis(100)).await;

        // Edit Style again on the same cell comes back to the same tab.
        tab.select(vec!["a".to_string()]);
        app.reveal_page(&tab.page);
        glib::timeout_future(Duration::from_millis(100)).await;
        edit.emit_clicked();
        glib::timeout_future(Duration::from_millis(300)).await;
        println!(
            "bench diagram style again editors={} front={}",
            editors().len(),
            app.active().is_some_and(|t| Rc::ptr_eq(&t, &editor))
        );

        // The star: set the cell's style as the Default Style, then clear it, and Undo.
        app.reveal_page(&tab.page);
        tab.select(vec!["a".to_string()]);
        glib::timeout_future(Duration::from_millis(100)).await;
        if let Some(b) = button("Set Default Style") {
            b.emit_clicked();
        }
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench diagram style set {:?}", star());
        if let Some(b) = button("Clear Default Style") {
            b.emit_clicked();
        }
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench diagram style cleared {:?}", star());
        tab.undo();
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench diagram style undo_clear {:?}", star());

        // The cell deleted: its editor closes.
        tab.select(vec!["a".to_string()]);
        tab.delete();
        glib::timeout_future(Duration::from_millis(300)).await;
        println!("bench diagram style deleted editors={}", editors().len());
        bench_quit(&app);
    });
}
