//! The Outline pane following the caret.

use super::*;

/// Caret positions covering section changes and returns to the top, clamped to note length.
const WALK: [i32; 8] = [0, 6, 60, 120, 400, 0, 12, 190];

/// Follow [`WALK`] caret positions, then switch tabs and hide/show Outline to check
/// selection catch-up. `hold:<note>` prints input targets and pane changes for 20 s
/// while XTEST drives real input.
pub(super) fn bench_outline(app: &Rc<App>, arg: &str) {
    if let Some(rel) = arg.strip_prefix("index:") {
        return bench_outline_index(app, rel);
    }
    let (hold, note, other) = match arg.strip_prefix("hold:") {
        Some(note) => (true, note, ""),
        None => match arg.split_once(',') {
            Some((note, other)) => (false, note, other),
            None => return bench_quit(app),
        },
    };
    let other = other.to_string();
    app.open_path(note);
    app.show_pane("outline");
    let app = app.clone();
    // Long enough for the language layer's first answer, which is what fills the pane.
    glib::timeout_add_local_once(Duration::from_millis(1000), move || {
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        tab.view.grab_focus();
        if hold {
            return bench_outline_hold(&app, &tab);
        }
        bench_outline_walk(app, tab, other, 0);
    });
}

/// `index:<rel_code>` probes Rust/Python Outline fallback to the index. Print initial
/// rows/server status, then server answers 6 s later. Activate a row, follow the caret,
/// append a function and save; require refreshed symbols. Writes files; use `/tmp`.
fn bench_outline_index(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_OUTLINE=index");
    app.open_path(rel);
    app.show_pane("outline");
    let app = app.clone();
    glib::spawn_future_local(async move {
        let rows = |app: &Rc<App>, tab: &Rc<Tab>, when: &str| {
            let lines = app.sidebar.get().map(|s| s.outline_lines(None));
            let missing = tab.lang.support().and_then(|s| s.missing.clone());
            println!(
                "bench outline index {when} served={} missing={missing:?}",
                tab.lang.served()
            );
            for line in lines.unwrap_or_default() {
                println!("bench outline index {when} row {line}");
            }
        };
        let mut tab = None;
        for _ in 0..75 {
            glib::timeout_future(Duration::from_millis(200)).await;
            tab = app.active().filter(|t| !t.lang.outline().is_empty());
            if tab.is_some() {
                break;
            }
        }
        let Some(tab) = tab else {
            println!("bench outline index no_rows");
            return bench_quit(&app);
        };
        rows(&app, &tab, "first");
        glib::timeout_future(Duration::from_secs(6)).await;
        rows(&app, &tab, "later");

        let outline = tab.lang.outline();
        let last = outline.len() - 1;
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.outline_lines(Some(last));
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
        let mut end = caret;
        end.forward_to_line_end();
        let mut start = caret;
        start.set_line_offset(0);
        println!(
            "bench outline index jump row={:?} caret={}:{} line={:?}",
            outline[last].1,
            caret.line(),
            caret.line_offset(),
            tab.buffer.text(&start, &end, false).trim()
        );

        tab.view.grab_focus();
        let body = outline
            .iter()
            .find(|(_, name, _)| !name.starts_with("impl "));
        if let Some((_, _, at)) = body {
            let line = at.line as i32 + 1;
            tab.buffer
                .place_cursor(&tab.buffer.iter_at_line(line).expect("bench line"));
            glib::timeout_future(Duration::from_millis(250)).await;
            println!(
                "bench outline index follow {}",
                outline_state(&app, &tab, line)
            );
        }

        let added = match tab.rel().ends_with(".py") {
            true => "\ndef added_by_drill():\n    pass\n",
            false => "\nfn added_by_drill() {}\n",
        };
        tab.buffer.insert(&mut tab.buffer.end_iter(), added);
        let _ = WidgetExt::activate_action(&app.window, "win.save", None);
        for _ in 0..50 {
            glib::timeout_future(Duration::from_millis(200)).await;
            if tab
                .lang
                .outline()
                .iter()
                .any(|row| row.1 == "added_by_drill")
            {
                break;
            }
        }
        rows(&app, &tab, "saved");
        bench_quit(&app);
    });
}

/// One step of the walk, and the rest of the drill once the walk is over.
fn bench_outline_walk(app: Rc<App>, tab: Rc<Tab>, other: String, step: usize) {
    let Some(&line) = WALK.get(step) else {
        return bench_outline_away(app, tab, other);
    };
    let line = line.min(tab.buffer.line_count() - 1);
    tab.buffer
        .place_cursor(&tab.buffer.iter_at_line(line).expect("bench line"));
    // Past the caret hook's 100 ms.
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        println!("bench outline walk {}", outline_state(&app, &tab, line));
        bench_outline_walk(app, tab, other, step + 1);
    });
}

/// Another tab and back, then the pane out of sight while the caret moves.
fn bench_outline_away(app: Rc<App>, tab: Rc<Tab>, other: String) {
    app.open_path(&other);
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        app.reveal_page(&tab.page);
        tab.view.grab_focus();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
            println!("bench outline back {}", outline_state(&app, &tab, line));
            bench_outline_hidden(app, tab, 0);
        });
    });
}

/// The caret moved while the pane is out of sight, behind the Files pane and then with the whole
/// sidebar hidden: the row it had while hidden, which should not have moved, and once shown.
fn bench_outline_hidden(app: Rc<App>, tab: Rc<Tab>, step: i32) {
    match step {
        0 => app.show_pane("files"),
        1 => app.sidebar_column.set_visible(false),
        _ => return bench_outline_edit(app, tab, 0),
    }
    let line = tab.buffer.line_count() * (step + 1) / 3;
    tab.buffer
        .place_cursor(&tab.buffer.iter_at_line(line).expect("bench line"));
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        println!(
            "bench outline hidden{step} {}",
            outline_state(&app, &tab, line)
        );
        match step {
            0 => app.show_pane("outline"),
            _ => app.sidebar_column.set_visible(true),
        }
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            println!(
                "bench outline shown{step} {}",
                outline_state(&app, &tab, line)
            );
            bench_outline_hidden(app, tab, step + 1);
        });
    });
}

/// Keep Outline scroll/selection stable through a heading edit, insertion and undo
/// while the list is scrolled away from the caret. A subsequent caret move must resume
/// following the active section.
fn bench_outline_edit(app: Rc<App>, tab: Rc<Tab>, step: usize) {
    let buffer = tab.buffer.clone();
    let typed = |text: &str| {
        buffer.begin_user_action();
        buffer.insert_interactive_at_cursor(text, true);
        buffer.end_user_action();
    };
    let (name, wait) = match step {
        0 => {
            let rows = tab.lang.outline();
            let Some((_, _, at)) = rows.get(rows.len() * 2 / 3) else {
                return bench_quit(&app);
            };
            let mut end = buffer.iter_at_line(at.line as i32).expect("bench line");
            end.forward_to_line_end();
            buffer.place_cursor(&end);
            ("placed", 250)
        }
        1 => {
            let list = app
                .sidebar
                .get()
                .and_then(|s| s.outline_child())
                .and_then(|c| find_widget(&c, &|w| w.is::<gtk::ListView>()))
                .and_downcast::<gtk::ListView>()
                .expect("bench list");
            list.vadjustment().expect("bench adjustment").set_value(0.0);
            ("scrolled", 100)
        }
        2 => {
            typed(" typed");
            ("typed", 1500)
        }
        3 => {
            typed("\n## Added heading");
            ("added", 1500)
        }
        4 => {
            buffer.undo();
            buffer.undo();
            ("undone", 1500)
        }
        5 => {
            let mut next = buffer.iter_at_mark(&buffer.get_insert());
            next.forward_line();
            buffer.place_cursor(&next);
            ("moved", 250)
        }
        _ => return bench_quit(&app),
    };
    glib::timeout_add_local_once(Duration::from_millis(wait), move || {
        let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
        println!(
            "bench outline edit {name} {}",
            outline_state(&app, &tab, line)
        );
        bench_outline_edit(app, tab, step + 1);
    });
}

/// Print where to aim, then the pane's state whenever it changes.
fn bench_outline_hold(app: &Rc<App>, tab: &Rc<Tab>) {
    let aim = |widget: &gtk::Widget| {
        widget
            .compute_bounds(&app.window)
            .map(|b| format!("{:.0},{:.0}", b.x() + b.width() / 2.0, b.y() + 40.0))
    };
    let list = app.sidebar.get().and_then(|s| s.outline_child());
    println!(
        "bench outline aim editor={:?} list={:?}",
        aim(tab.view.upcast_ref()),
        list.as_ref().and_then(aim)
    );
    let (app, tab) = (app.clone(), tab.clone());
    let last = RefCell::new(String::new());
    let started = Instant::now();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
        let state = outline_state(&app, &tab, line);
        if *last.borrow() != state {
            println!("bench outline hold {state}");
            *last.borrow_mut() = state;
        }
        if started.elapsed() < Duration::from_secs(20) {
            return glib::ControlFlow::Continue;
        }
        bench_quit(&app);
        glib::ControlFlow::Break
    });
}

/// Print caret line, enclosing heading, selected row, row visibility and focus.
/// Compare with `placed` to detect selection incorrectly activating a heading.
fn outline_state(app: &Rc<App>, tab: &Rc<Tab>, placed: i32) -> String {
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    let focus = match gtk::prelude::GtkWindowExt::focus(&app.window) {
        Some(w) if &w == tab.view.upcast_ref::<gtk::Widget>() => "editor".to_string(),
        Some(w) => w.type_().name().to_string(),
        None => "none".to_string(),
    };
    let list = app
        .sidebar
        .get()
        .and_then(|s| s.outline_child())
        .and_then(|c| find_widget(&c, &|w| w.is::<gtk::ListView>()))
        .and_downcast::<gtk::ListView>();
    let Some(list) = list else {
        return format!("line={line} list=none focus={focus}");
    };
    let selection = list
        .model()
        .and_downcast::<gtk::SingleSelection>()
        .expect("bench selection");
    let row = selection.selected();
    let text = selection
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|s| s.string().to_string());
    // Every row is one line of the same label, so a row's place is its index times the height.
    let adj = list.vadjustment().expect("bench adjustment");
    let (value, page, upper) = (adj.value(), adj.page_size(), adj.upper());
    let height = upper / f64::from(selection.n_items().max(1));
    let top = f64::from(row) * height;
    let in_view = text.is_some() && top >= value - 0.5 && top + height <= value + page + 0.5;
    format!(
        "line={line} caret_kept={} under={:?} row={} text={text:?} in_view={in_view} \
         scroll={value:.0}/{upper:.0} page={page:.0} focus={focus}",
        line == placed,
        heading_above(tab, line),
        text.as_ref().map_or(-1, |_| row as i64),
    )
}

/// The heading a line of a note is under, read off the text: the last `#` line at or above it.
fn heading_above(tab: &Rc<Tab>, line: i32) -> Option<String> {
    let text = tab.text();
    text.lines()
        .take(line as usize + 1)
        .filter(|l| l.starts_with('#'))
        .last()
        .map(|l| l.trim_start_matches('#').trim().to_string())
}
